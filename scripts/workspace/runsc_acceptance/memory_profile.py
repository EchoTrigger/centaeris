"""Bounded optional resident-memory work and tenant admission budget."""

GIB = 1024 ** 3
GLOBAL_MEMORY_BUDGET_BYTES = 12 * GIB


def memory_profile(environments, iterations, memory_mib=0, minimum_hold_ms=0):
    if type(memory_mib) is not int or type(minimum_hold_ms) is not int:
        raise ValueError('bounded integer memory and hold values required')
    if not 0 <= memory_mib <= 1536 or not 0 <= minimum_hold_ms <= 120000:
        raise ValueError('bounded memory (0..1536 MiB) and hold (0..120000 ms) required')
    if bool(memory_mib) != bool(minimum_hold_ms):
        raise ValueError('memory and minimum hold must be paired')
    if memory_mib:
        if type(environments) is not int or not 2 <= environments <= 64:
            raise ValueError('memory profile requires at least two environments (maximum 64)')
        if type(iterations) is not int or iterations != 1:
            raise ValueError('memory profile requires exactly one iteration')
    return {'memoryMiB': memory_mib, 'minimumHoldMs': minimum_hold_ms}


def workspace_memory_budget(environments, value=None):
    if value is None:
        return (12 if environments >= 8 else 8) * GIB
    if type(value) is not int or not 4 * GIB <= value <= GLOBAL_MEMORY_BUDGET_BYTES:
        raise ValueError('workspace memory budget must be between 4 GiB and the 12 GiB global budget')
    return value
