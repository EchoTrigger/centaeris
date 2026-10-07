from datetime import timedelta
from urllib.parse import urlencode

from django.conf import settings
from django.contrib.auth import get_user_model
from django.contrib.auth.password_validation import validate_password
from django.contrib.auth.tokens import default_token_generator
from django.core.exceptions import ValidationError
from django.core.mail import send_mail
from django.core.validators import validate_email
from django.db import IntegrityError, transaction
from django.utils import timezone
from django.utils.crypto import salted_hmac
from django.utils.encoding import force_bytes, force_str
from django.utils.http import urlsafe_base64_decode, urlsafe_base64_encode

from app_core.models import PasswordResetMail


EMAIL_RATE_LIMIT_SECONDS = 60
EMAIL_RATE_LIMIT_PER_HOUR = 5
IP_RATE_LIMIT_PER_HOUR = 100


def _digest(kind: str, value: str) -> str:
    digest = salted_hmac(
        f"app_core.password_reset.{kind}",
        value,
        secret=settings.SECRET_KEY,
        algorithm="sha256",
    ).hexdigest()
    return f"sha256:{digest}"


def password_state_digest(user) -> str:
    return _digest("password_state", user.password)


def queue_password_reset(email: str, client_address: str) -> None:
    normalized_email = email.strip().lower() if isinstance(email, str) else ""
    try:
        validate_email(normalized_email)
    except ValidationError:
        return

    now = timezone.now()
    hour_ago = now - timedelta(hours=1)
    email_digest = _digest("email", normalized_email)
    ip_digest = _digest("ip", client_address or "unknown")
    recent = PasswordResetMail.objects.filter(requested_at__gte=hour_ago)
    if (
        recent.filter(
            email_digest=email_digest,
            requested_at__gte=now - timedelta(seconds=EMAIL_RATE_LIMIT_SECONDS),
        ).exists()
        or recent.filter(email_digest=email_digest).count()
        >= EMAIL_RATE_LIMIT_PER_HOUR
        or recent.filter(ip_digest=ip_digest).count() >= IP_RATE_LIMIT_PER_HOUR
    ):
        return

    user = (
        get_user_model()
        .objects.filter(username__iexact=normalized_email)
        .first()
    )
    eligible = bool(user and user.is_active and user.has_usable_password())
    if not eligible:
        PasswordResetMail.objects.create(
            email_digest=email_digest,
            ip_digest=ip_digest,
            status="suppressed",
            next_attempt_at=now,
        )
        return

    try:
        with transaction.atomic():
            PasswordResetMail.objects.create(
                user=user,
                email_digest=email_digest,
                ip_digest=ip_digest,
                password_state_digest=password_state_digest(user),
                next_attempt_at=now,
            )
    except IntegrityError:
        # A concurrent or still-pending request already owns this delivery.
        return


def _password_reset_link(user) -> str:
    fragment = urlencode(
        {
            "uid": urlsafe_base64_encode(force_bytes(user.pk)),
            "token": default_token_generator.make_token(user),
        }
    )
    return f"{settings.WEB_ORIGIN.rstrip('/')}/reset-password#{fragment}"


def _password_reset_validity_label() -> str:
    seconds = settings.PASSWORD_RESET_TIMEOUT
    if seconds % 3600 == 0:
        return f"{seconds // 3600} 小时"
    return f"{seconds // 60} 分钟"


def process_next_password_reset_mail() -> bool:
    with transaction.atomic():
        mail = (
            PasswordResetMail.objects.select_for_update(skip_locked=True)
            .filter(status="pending", next_attempt_at__lte=timezone.now())
            .order_by("next_attempt_at", "id")
            .first()
        )
        if mail is None:
            return False

        user = mail.user
        recipient = (user.email or user.username).strip() if user else ""
        try:
            validate_email(recipient)
        except ValidationError:
            recipient = ""
        if (
            not user
            or not user.is_active
            or not user.has_usable_password()
            or not recipient
            or mail.password_state_digest != password_state_digest(user)
        ):
            mail.status = "suppressed"
            mail.save(update_fields=["status"])
            return True

        now = timezone.now()
        mail.attempt_count += 1
        mail.last_attempt_at = now
        try:
            send_mail(
                subject="重置您的 Centaeris 密码",
                message=(
                    "我们收到了重置 Centaeris 密码的请求。\n\n"
                    f"请在 {_password_reset_validity_label()}内打开以下链接：\n"
                    f"{_password_reset_link(user)}\n\n"
                    "如果这不是您的操作，请忽略此邮件。"
                ),
                from_email=settings.DEFAULT_FROM_EMAIL,
                recipient_list=[recipient],
                fail_silently=False,
            )
        except Exception as error:
            retry_seconds = min(60 * (2 ** min(mail.attempt_count - 1, 6)), 3600)
            mail.next_attempt_at = now + timedelta(seconds=retry_seconds)
            mail.last_error_kind = type(error).__name__[:160]
            mail.save(
                update_fields=[
                    "attempt_count",
                    "last_attempt_at",
                    "last_error_kind",
                    "next_attempt_at",
                ]
            )
            return True

        mail.status = "sent"
        mail.sent_at = now
        mail.last_error_kind = ""
        mail.save(
            update_fields=[
                "status",
                "attempt_count",
                "last_attempt_at",
                "last_error_kind",
                "sent_at",
            ]
        )
        return True


@transaction.atomic
def reset_password(uid: str, token: str, new_password: str) -> str | None:
    try:
        user_id = force_str(urlsafe_base64_decode(uid))
        user = get_user_model().objects.select_for_update().get(pk=user_id, is_active=True)
    except (TypeError, ValueError, OverflowError, get_user_model().DoesNotExist):
        return "account_password_reset_invalid"
    if not default_token_generator.check_token(user, token):
        return "account_password_reset_invalid"
    if user.check_password(new_password):
        return "account_password_unchanged"
    try:
        validate_password(new_password, user=user)
    except ValidationError:
        return "account_password_invalid"

    user.set_password(new_password)
    user.save(update_fields=["password"])
    PasswordResetMail.objects.filter(user=user, status="pending").update(
        status="suppressed"
    )
    return None
