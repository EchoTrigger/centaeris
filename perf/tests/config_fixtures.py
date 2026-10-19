"""Current native execution bindings for synthetic performance Compose responses."""


def configure_execution_host(service):
    helper = '/var/lib/centaeris-perf/host/execution_agent'
    service.setdefault('environment', {}).update(
        ENVIRONMENT_EXECUTION_HELPER=helper,
        ENVIRONMENT_HOST_SOCKET='/run/centaeris-host/control.sock')
    service.setdefault('volumes', []).extend([
        {'type': 'bind', 'source': '/run/centaeris-perf-host',
         'target': '/run/centaeris-host', 'read_only': True},
        {'type': 'bind', 'source': helper, 'target': helper, 'read_only': True},
    ])
