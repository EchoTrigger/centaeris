"""One session-pinned PostgreSQL listener owned by a presentation hub."""
import asyncio
import logging
from dataclasses import dataclass

import psycopg
from psycopg import sql

from .presentation_changes import CHANNEL, PROJECTED_CHANNEL


logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class PresentationHint:
    key: str | None
    needs_drain: bool


async def listen_presentation_changes(
    ready, *, connection_parameters, connect=None, wait=asyncio.sleep,
    reconcile_after=30, max_reconnects=5,
):
    """NOTIFY is a wake hint; an empty socket wait asks for durable reconciliation.

    The independent autocommit connection deliberately bypasses Django's request
    transaction pool. Repeated LISTEN success never resets the reconnect budget.
    The caller owns this generator and closes it with its last subscription.
    """
    if type(max_reconnects) is not int or not 0 <= max_reconnects <= 5:
        raise ValueError("presentation listener reconnect budget invalid")
    if not isinstance(reconcile_after, (int, float)) or not 0 < reconcile_after <= 30:
        raise ValueError("presentation listener reconciliation interval invalid")
    connect = connect or psycopg.AsyncConnection.connect
    for attempt in range(max_reconnects + 1):
        connection = None
        try:
            parameters = {**connection_parameters(), "autocommit": True, "connect_timeout": 5}
            connection = await asyncio.wait_for(connect(**parameters), 5)
            for channel in (CHANNEL, PROJECTED_CHANNEL):
                await asyncio.wait_for(connection.execute(sql.SQL("LISTEN {}").format(sql.Identifier(channel))), 5)
            ready.set()
            yield PresentationHint(None, True)
            while True:
                received = False
                async for notification in connection.notifies(timeout=reconcile_after):
                    if notification.channel not in (CHANNEL, PROJECTED_CHANNEL):
                        continue
                    key = notification.payload
                    if not isinstance(key, str) or not key or len(key) > 256:
                        continue
                    received = True
                    yield PresentationHint(key, notification.channel == CHANNEL)
                if not received:
                    yield PresentationHint(None, True)
        except asyncio.CancelledError:
            raise
        except (psycopg.Error, OSError, TimeoutError) as error:
            if attempt >= max_reconnects:
                raise RuntimeError("presentation change listener unavailable") from error
        finally:
            if connection is not None:
                try:
                    await asyncio.wait_for(connection.close(), 5)
                except Exception as error:
                    logger.error("Presentation notification connection close failed",
                                 extra={"exceptionType": type(error).__name__})
                    raise RuntimeError("presentation notification connection close failed") from error
        await wait(min(.5 * 2 ** attempt, 8))
