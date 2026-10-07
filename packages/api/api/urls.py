from django.urls import path

from api.ninja_api import api


urlpatterns = [
    path("", api.urls),
]
