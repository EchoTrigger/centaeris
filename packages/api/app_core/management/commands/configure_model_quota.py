from django.core.management.base import BaseCommand, CommandError
from django.db import transaction

from app_core.models import ModelQuotaDomain, ProviderCredential


class Command(BaseCommand):
    help = "Bind a provider credential to an explicit shared model quota domain."

    def add_arguments(self, parser):
        parser.add_argument("--domain-id", required=True)
        parser.add_argument("--max-concurrent", type=int, required=True)
        parser.add_argument("--provider-id", required=True)
        parser.add_argument("--enable", action="store_true")

    def handle(self, *args, **options):
        domain_id = options["domain_id"]
        limit = options["max_concurrent"]
        if not domain_id.strip() or len(domain_id) > 64 or not 1 <= limit <= 64:
            raise CommandError("Quota domain ID or concurrency limit is invalid")
        with transaction.atomic():
            domain, created = ModelQuotaDomain.objects.select_for_update().get_or_create(
                id=domain_id,
                defaults={"maxConcurrent": limit, "enabled": options["enable"]},
            )
            if not created and domain.maxConcurrent != limit:
                raise CommandError("Existing quota domain has a different limit")
            if options["enable"] and not domain.enabled:
                domain.enabled = True
                domain.save(update_fields=["enabled", "updatedAt"])
            try:
                credential = ProviderCredential.objects.select_for_update().get(
                    provider_id=options["provider_id"]
                )
            except ProviderCredential.DoesNotExist as error:
                raise CommandError("Provider credential not found") from error
            credential.quotaDomain = domain
            credential.save(update_fields=["quotaDomain", "updatedAt"])
        self.stdout.write(f"{options['provider_id']} -> {domain_id} (enabled={domain.enabled})")
