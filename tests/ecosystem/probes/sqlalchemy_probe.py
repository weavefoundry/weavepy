"""Ecosystem probe: sqlalchemy — Core table create/insert/select over
sqlite3, then a declarative ORM model with session commit + query, then
(RFC 0066 WS4) the asyncio extension: AsyncSession over aiosqlite rides
`greenlet_spawn`, the Python -> C -> Python stack-switch shape the
bundled native greenlet exists for."""

import sqlalchemy
from sqlalchemy import (
    Column,
    Integer,
    MetaData,
    String,
    Table,
    create_engine,
    insert,
    select,
)
from sqlalchemy.orm import Session, declarative_base

# --- Core ----------------------------------------------------------------
engine = create_engine("sqlite://")
metadata = MetaData()
users = Table(
    "users",
    metadata,
    Column("id", Integer, primary_key=True),
    Column("name", String(50), nullable=False),
)
metadata.create_all(engine)

with engine.begin() as conn:
    conn.execute(insert(users), [{"name": "ada"}, {"name": "grace"}])

with engine.connect() as conn:
    rows = conn.execute(select(users).order_by(users.c.name)).all()
    assert [r.name for r in rows] == ["ada", "grace"], rows

# --- ORM -----------------------------------------------------------------
Base = declarative_base()


class Language(Base):
    __tablename__ = "languages"
    id = Column(Integer, primary_key=True)
    name = Column(String(50))
    year = Column(Integer)


orm_engine = create_engine("sqlite://")
Base.metadata.create_all(orm_engine)

with Session(orm_engine) as session:
    session.add_all(
        [Language(name="python", year=1991), Language(name="rust", year=2015)]
    )
    session.commit()

with Session(orm_engine) as session:
    rust = session.query(Language).filter_by(name="rust").one()
    assert rust.year == 2015, rust.year
    count = session.query(Language).filter(Language.year < 2000).count()
    assert count == 1, count

# --- asyncio extension (greenlet_spawn) -----------------------------------
import asyncio

from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine


async def async_leg():
    aengine = create_async_engine("sqlite+aiosqlite://")
    async with aengine.begin() as conn:
        await conn.run_sync(Base.metadata.create_all)
    async with AsyncSession(aengine) as session:
        session.add_all(
            [Language(name="haskell", year=1990), Language(name="go", year=2009)]
        )
        await session.commit()
    async with AsyncSession(aengine) as session:
        result = await session.execute(
            select(Language).where(Language.year < 2000).order_by(Language.name)
        )
        names = [lang.name for lang in result.scalars()]
        assert names == ["haskell"], names
    await aengine.dispose()


asyncio.run(async_leg())

print("sqlalchemy ok", sqlalchemy.__version__)
