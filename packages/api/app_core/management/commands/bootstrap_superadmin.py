import os

from django.contrib.auth import get_user_model
from django.contrib.auth.password_validation import validate_password
from django.core.exceptions import ValidationError
from django.core.management.base import BaseCommand, CommandError
from django.core.validators import validate_email
from django.db import transaction

from app_core.models import (
    Workspace,
    WorkspaceGroup,
    WorkspaceMembership,
)
from app_core.agent_defaults import ensure_default_agent


DEFAULT_WORKSPACE_NAME = "Default"
DEFAULT_WORKSPACE_GROUP_NAME = "全体成员"


class Command(BaseCommand):
    help = "Create the deployment's bootstrap superadmin and default workspace."

    def handle(self, *args, **options):
        email = required_bootstrap_value("BOOTSTRAP_SUPERADMIN_EMAIL")
        password = required_bootstrap_value("BOOTSTRAP_SUPERADMIN_PASSWORD")
        validate_bootstrap_email(email)
        validate_bootstrap_password(password)

        User = get_user_model()
        with transaction.atomic():
            user = User.objects.select_for_update().filter(username=email).first()
            if user is not None and (
                not user.is_active
                or not user.is_staff
                or not user.is_superuser
                or user.email != email
            ):
                raise CommandError(
                    "BOOTSTRAP_SUPERADMIN_EMAIL already belongs to an account that "
                    "is not the active bootstrap superadmin"
                )
            if (
                Workspace.objects.select_for_update()
                .filter(name=DEFAULT_WORKSPACE_NAME)
                .exclude(createdBy_id=user.id if user else None)
                .exists()
            ):
                raise CommandError(
                    "Default workspace already belongs to a different bootstrap superadmin"
                )
            if user is None:
                user = User.objects.create_superuser(
                    username=email,
                    email=email,
                    password=password,
                )
                user_created = True
            else:
                user_created = False

            workspace, workspace_created = Workspace.objects.get_or_create(
                createdBy=user,
                name=DEFAULT_WORKSPACE_NAME,
            )
            membership, membership_created = WorkspaceMembership.objects.get_or_create(
                workspace=workspace,
                user=user,
                defaults={"role": "owner"},
            )
            if membership.role != "owner":
                raise CommandError(
                    "Bootstrap superadmin must own the default workspace"
                )
            workspace_group, workspace_group_created = WorkspaceGroup.objects.get_or_create(
                workspace=workspace,
                kind="all_members",
                defaults={
                    "name": DEFAULT_WORKSPACE_GROUP_NAME,
                    "createdBy": user,
                },
            )
            if workspace_group.name != DEFAULT_WORKSPACE_GROUP_NAME:
                raise CommandError(
                    "Default workspace all-members group must use the exact system name"
                )
            agent, agent_created = ensure_default_agent(workspace, user)

        if user_created:
            self.stdout.write(self.style.SUCCESS(f"Created bootstrap superadmin {email}"))
        if workspace_created:
            self.stdout.write(self.style.SUCCESS(f"Created default workspace {workspace.id}"))
        if membership_created:
            self.stdout.write(
                self.style.SUCCESS(f"Created default workspace owner membership {membership.id}")
            )
        if workspace_group_created:
            self.stdout.write(self.style.SUCCESS(f"Created default workspace group {workspace_group.id}"))
        if agent_created:
            self.stdout.write(self.style.SUCCESS(f"Created default Agent {agent.id}"))
        if not any(
            (
                user_created,
                workspace_created,
                membership_created,
                workspace_group_created,
                agent_created,
            )
        ):
            self.stdout.write(self.style.SUCCESS(f"Verified bootstrap superadmin and default workspace for {email}"))


def required_bootstrap_value(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise CommandError(f"Missing required environment variable: {name}")
    if value != value.strip():
        raise CommandError(f"{name} must not contain leading or trailing whitespace")
    return value


def validate_bootstrap_email(email: str) -> None:
    try:
        validate_email(email)
    except ValidationError as error:
        raise CommandError("BOOTSTRAP_SUPERADMIN_EMAIL must be a valid email address") from error


def validate_bootstrap_password(password: str) -> None:
    try:
        validate_password(password)
    except ValidationError as error:
        raise CommandError("BOOTSTRAP_SUPERADMIN_PASSWORD does not meet the configured password policy") from error
