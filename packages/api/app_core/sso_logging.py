"""Keep authorization codes and state out of ASGI access logs."""
import logging
import re


class OAuthCallbackFilter(logging.Filter):
    def filter(self, record):
        if isinstance(record.args, tuple) and len(record.args) == 5:
            address, method, target, version, status = record.args
            if isinstance(target, str) and re.match(r"^/api/sso/[^/]+/callback(?:\?|$)", target):
                record.args = (address, method, target.split("?", 1)[0], version, status)
        return True


def configure_oauth_logging():
    logger = logging.getLogger("uvicorn.access")
    if not any(isinstance(item, OAuthCallbackFilter) for item in logger.filters):
        logger.addFilter(OAuthCallbackFilter())
    # Authlib's debug transport logs include PKCE verifiers and token responses.
    logging.getLogger("authlib").setLevel(logging.WARNING)
