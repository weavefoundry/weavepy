"""Ecosystem probe: alembic (RFC 0076 WS5) — migrations over
sqlalchemy + sqlite, end to end: `alembic init` a scratch project,
autogenerate a revision from a declarative model (the diff engine),
`upgrade head`, assert the schema and a data round-trip, autogenerate
a second revision after a model change (add-column diff), upgrade,
then `downgrade -1` and assert the column is gone. Exercises the
migration-script exec path — alembic compiles + execs the generated
revision files — plus sqlalchemy DDL over the RFC 0056 sqlite3."""

import faulthandler
import os
import sys
import tempfile

faulthandler.enable()


def stage(name: str) -> None:
    print(f"[stage] {name}", file=sys.stderr, flush=True)


stage("import")
import sqlalchemy as sa
from alembic import command
from alembic.config import Config
from alembic.script import ScriptDirectory

import alembic

scratch = tempfile.mkdtemp(prefix="weavepy_alembic_")
os.chdir(scratch)
db_url = "sqlite:///" + os.path.join(scratch, "probe.db")

# --- alembic init -------------------------------------------------------------
stage("init")
cfg = Config(os.path.join(scratch, "alembic.ini"))
cfg.set_main_option("script_location", os.path.join(scratch, "migrations"))
cfg.set_main_option("sqlalchemy.url", db_url)
command.init(cfg, os.path.join(scratch, "migrations"))

# Point env.py at a target_metadata we control (autogenerate needs it).
env_path = os.path.join(scratch, "migrations", "env.py")
with open(env_path, encoding="utf-8") as f:
    env_src = f.read()
env_src = env_src.replace(
    "target_metadata = None",
    "import probe_models\ntarget_metadata = probe_models.metadata",
)
with open(env_path, "w", encoding="utf-8") as f:
    f.write(env_src)

# --- model v1 -------------------------------------------------------------------
stage("model-v1")
model_v1 = '''
import sqlalchemy as sa

metadata = sa.MetaData()

users = sa.Table(
    "users",
    metadata,
    sa.Column("id", sa.Integer, primary_key=True),
    sa.Column("name", sa.String(50), nullable=False),
)
'''
with open(os.path.join(scratch, "probe_models.py"), "w", encoding="utf-8") as f:
    f.write(model_v1)
sys.path.insert(0, scratch)

# --- autogenerate + upgrade head --------------------------------------------------
stage("revision-1")
command.revision(cfg, message="create users", autogenerate=True)
script_dir = ScriptDirectory.from_config(cfg)
head_1 = script_dir.get_current_head()
assert head_1 is not None
rev_file = script_dir.get_revision(head_1).path
with open(rev_file, encoding="utf-8") as f:
    rev_src = f.read()
assert "create_table" in rev_src and "users" in rev_src, rev_src

stage("upgrade-1")
command.upgrade(cfg, "head")
engine = sa.create_engine(db_url)
insp = sa.inspect(engine)
assert "users" in insp.get_table_names(), insp.get_table_names()

# Data round-trip through the migrated schema.
metadata = sa.MetaData()
users = sa.Table("users", metadata, autoload_with=engine)
with engine.begin() as conn:
    conn.execute(users.insert(), [{"name": "ada"}, {"name": "grace"}])
    names = [r.name for r in conn.execute(sa.select(users).order_by(users.c.id))]
assert names == ["ada", "grace"], names
engine.dispose()

# --- model v2: add a column, autogenerate the diff --------------------------------
stage("model-v2")
model_v2 = model_v1.replace(
    'sa.Column("name", sa.String(50), nullable=False),',
    'sa.Column("name", sa.String(50), nullable=False),\n'
    '    sa.Column("email", sa.String(120), nullable=True),',
)
with open(os.path.join(scratch, "probe_models.py"), "w", encoding="utf-8") as f:
    f.write(model_v2)
import importlib

import probe_models

importlib.reload(probe_models)

stage("revision-2")
command.revision(cfg, message="add email", autogenerate=True)
script_dir = ScriptDirectory.from_config(cfg)
head_2 = script_dir.get_current_head()
assert head_2 != head_1
rev2_src = open(script_dir.get_revision(head_2).path, encoding="utf-8").read()
assert "add_column" in rev2_src and "email" in rev2_src, rev2_src

stage("upgrade-2")
command.upgrade(cfg, "head")
engine = sa.create_engine(db_url)
cols = {c["name"] for c in sa.inspect(engine).get_columns("users")}
assert cols == {"id", "name", "email"}, cols
engine.dispose()

# --- downgrade -1: the column must be gone, the data must survive ------------------
stage("downgrade")
command.downgrade(cfg, "-1")
engine = sa.create_engine(db_url)
cols = {c["name"] for c in sa.inspect(engine).get_columns("users")}
assert cols == {"id", "name"}, cols
with engine.connect() as conn:
    metadata = sa.MetaData()
    users = sa.Table("users", metadata, autoload_with=engine)
    count = conn.execute(sa.select(sa.func.count()).select_from(users)).scalar()
assert count == 2, count
engine.dispose()

stage("done")
print("alembic ok", alembic.__version__)
