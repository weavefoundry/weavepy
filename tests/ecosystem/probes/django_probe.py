"""Ecosystem probe: django capstone (RFC 0056 WS6) — a miniature project
configured in-process: sqlite3 backend, a one-model app, migrate, ORM
CRUD + aggregation + transaction rollback, then a request/response cycle
through django.test.Client against a view that queries the model."""

import os
import sys
import tempfile

import django
from django.conf import settings

scratch = tempfile.mkdtemp(prefix="weavepy_django_")

settings.configure(
    DEBUG=True,
    SECRET_KEY="probe-secret",
    ALLOWED_HOSTS=["testserver"],
    DATABASES={
        "default": {
            "ENGINE": "django.db.backends.sqlite3",
            "NAME": os.path.join(scratch, "db.sqlite3"),
        }
    },
    INSTALLED_APPS=[
        "django.contrib.contenttypes",
        "django.contrib.auth",
    ],
    ROOT_URLCONF=sys.modules[__name__],
    USE_TZ=True,
)
django.setup()

# --- one-model app ---------------------------------------------------------
from django.db import connection, models, transaction  # noqa: E402


class Language(models.Model):
    name = models.CharField(max_length=50, unique=True)
    year = models.IntegerField()

    class Meta:
        app_label = "probeapp"


# Materialize the schema directly (the startproject/migrate equivalent for
# an in-process app without a migrations package).
with connection.schema_editor() as editor:
    editor.create_model(Language)

# The built-in apps' migrations still run end-to-end.
from django.core.management import call_command  # noqa: E402

call_command("migrate", run_syncdb=True, verbosity=0, interactive=False)

# --- ORM CRUD ---------------------------------------------------------------
Language.objects.create(name="python", year=1991)
Language.objects.create(name="rust", year=2015)
Language.objects.create(name="go", year=2009)

assert Language.objects.count() == 3
assert Language.objects.filter(year__gte=2000).count() == 2
rust = Language.objects.get(name="rust")
rust.year = 2016
rust.save(update_fields=["year"])
assert Language.objects.get(name="rust").year == 2016
Language.objects.filter(name="go").delete()
assert sorted(Language.objects.values_list("name", flat=True)) == ["python", "rust"]

# aggregation
from django.db.models import Max  # noqa: E402

assert Language.objects.aggregate(Max("year"))["year__max"] == 2016

# --- transaction rollback ----------------------------------------------------
try:
    with transaction.atomic():
        Language.objects.create(name="zig", year=2016)
        assert Language.objects.count() == 3
        raise RuntimeError("force rollback")
except RuntimeError:
    pass
assert Language.objects.count() == 2, "atomic() rollback failed"

# --- request/response through the test client -------------------------------
from django.http import JsonResponse  # noqa: E402
from django.urls import path  # noqa: E402


def language_list(request):
    rows = list(Language.objects.order_by("name").values("name", "year"))
    return JsonResponse({"languages": rows})


urlpatterns = [path("languages/", language_list)]

from django.test import Client  # noqa: E402

client = Client()
resp = client.get("/languages/")
assert resp.status_code == 200, resp.status_code
body = resp.json()
assert body == {
    "languages": [
        {"name": "python", "year": 1991},
        {"name": "rust", "year": 2016},
    ]
}, body

resp = client.get("/nope/")
assert resp.status_code == 404

print("django ok", django.get_version())
