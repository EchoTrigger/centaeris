"""The WAL fence shared by host-side resource liability reservations."""


def require_durable_commit(database, error):
    if database.vendor != "postgresql":
        return
    with database.cursor() as cursor:
        cursor.execute("SHOW fsync")
        if cursor.fetchone()[0] != "on":
            raise error
        cursor.execute("SET LOCAL synchronous_commit = on")
