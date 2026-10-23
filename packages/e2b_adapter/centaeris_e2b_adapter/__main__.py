import argparse
import os
import sys

from .bridge import Adapter, AdapterConfig, serve


def main():
    parser = argparse.ArgumentParser(description="Trusted E2B SDK adapter")
    parser.add_argument("--lane", choices=("ordinary", "control", "cleanup"), required=True)
    args = parser.parse_args()
    try:
        config = AdapterConfig(api_key=os.environ.get("E2B_API_KEY", ""), domain=os.environ.get("E2B_DOMAIN"),
                               api_url=os.environ.get("E2B_API_URL"), sandbox_url=os.environ.get("E2B_SANDBOX_URL"),
                               spool_root=os.environ.get("E2B_ADAPTER_SPOOL_ROOT"),
                               idempotency_supported=os.environ.get("E2B_ADAPTER_IDEMPOTENCY_SUPPORTED") == "true", lane=args.lane,
                               power_observations=os.environ.get("CENTAERIS_E2B_POWER_OBSERVATIONS") == "1")
        adapter = Adapter(config)
    except Exception:
        print("E2B adapter startup failed: invalid configuration or unavailable pinned SDK", file=sys.stderr)
        return 1
    serve(adapter, sys.stdin.buffer, sys.stdout.buffer)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
