"""Django's database session backend with explicit, nullable login deadlines."""
from copy import deepcopy
import logging

from asgiref.sync import sync_to_async
from django.contrib.auth import SESSION_KEY
from django.contrib.sessions.backends.base import UpdateError
from django.contrib.sessions.backends.db import SessionStore as DatabaseSessionStore
from django.core.exceptions import SuspiciousOperation
from django.db import router
from django.db.models import Q
from django.utils import timezone


class SessionStore(DatabaseSessionStore):
    @classmethod
    def get_model_class(cls):
        from .models import BrowserLoginSession
        return BrowserLoginSession

    def _get_session_from_db(self):
        try:
            instance = self.model.objects.filter(
                Q(expire_date__isnull=True) | Q(expire_date__gt=timezone.now()),
                session_key=self.session_key,
            ).get()
            self._loaded_permanent = instance.expire_date is None
            return instance
        except (self.model.DoesNotExist, SuspiciousOperation) as error:
            if isinstance(error, SuspiciousOperation):
                logging.getLogger("django.security.%s" % error.__class__.__name__).warning(str(error))
            self._loaded_permanent = False
            self._session_key = None

    async def _aget_session_from_db(self):
        return await sync_to_async(self._get_session_from_db, thread_sensitive=True)()

    def load(self):
        data = super().load()
        self._loaded_data = deepcopy(data)
        return data

    async def aload(self):
        data = await super().aload()
        self._loaded_data = deepcopy(data)
        return data

    def _unchanged_permanent_login(self, data):
        return (getattr(self, "_loaded_permanent", False) and SESSION_KEY in data
                and "_session_expiry" not in data and data == self._loaded_data)

    def save(self, must_create=False):
        data = self._get_session(no_load=must_create)
        if not must_create and self._unchanged_permanent_login(data):
            # Django renews the browser cookie on every request. The permanent
            # server record has no deadline to renew and a read need not write it.
            using = router.db_for_write(self.model)
            if not self.model.objects.using(using).filter(session_key=self.session_key, expire_date__isnull=True).exists():
                raise UpdateError
            return
        return super().save(must_create=must_create)

    async def asave(self, must_create=False):
        data = await self._aget_session(no_load=must_create)
        if not must_create and self._unchanged_permanent_login(data):
            using = router.db_for_write(self.model)
            if not await self.model.objects.using(using).filter(session_key=self.session_key, expire_date__isnull=True).aexists():
                raise UpdateError
            return
        return await super().asave(must_create=must_create)

    def create_model_instance(self, data):
        instance = super().create_model_instance(data)
        if SESSION_KEY in data and "_session_expiry" not in data:
            instance.expire_date = None
        return instance

    async def acreate_model_instance(self, data):
        instance = await super().acreate_model_instance(data)
        if SESSION_KEY in data and "_session_expiry" not in data:
            instance.expire_date = None
        return instance
