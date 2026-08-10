"""Ecosystem probe: fastapi capstone (RFC 0058 WS8) — routing, pydantic
request/response models, path/query validation, dependency injection,
and an async endpoint, all through fastapi.testclient (httpx + starlette
in the chain)."""

from typing import Optional

from fastapi import Depends, FastAPI, HTTPException
from fastapi.testclient import TestClient
from pydantic import BaseModel

app = FastAPI()

DB: dict[int, dict] = {}


class Item(BaseModel):
    name: str
    price: float
    tags: list[str] = []


def get_db() -> dict[int, dict]:
    return DB


@app.post("/items/{item_id}", status_code=201)
def create_item(item_id: int, item: Item, db: dict = Depends(get_db)):
    if item_id in db:
        raise HTTPException(status_code=409, detail="exists")
    db[item_id] = item.model_dump()
    return {"id": item_id, **db[item_id]}


@app.get("/items/{item_id}")
def read_item(item_id: int, q: Optional[str] = None, db: dict = Depends(get_db)):
    if item_id not in db:
        raise HTTPException(status_code=404, detail="not found")
    out = {"id": item_id, **db[item_id]}
    if q:
        out["q"] = q
    return out


@app.get("/async-sum")
async def async_sum(a: int, b: int):
    return {"total": a + b}


client = TestClient(app)

# create + read round-trip through the pydantic model
resp = client.post("/items/1", json={"name": "widget", "price": 9.5, "tags": ["a"]})
assert resp.status_code == 201, (resp.status_code, resp.text)
assert resp.json() == {"id": 1, "name": "widget", "price": 9.5, "tags": ["a"]}

resp = client.get("/items/1", params={"q": "hello"})
assert resp.status_code == 200
assert resp.json()["q"] == "hello"

# validation: non-int path param is a 422 from pydantic
resp = client.get("/items/not-an-int")
assert resp.status_code == 422, resp.status_code
detail = resp.json()["detail"]
assert detail and detail[0]["type"] == "int_parsing", detail

# body validation failure
resp = client.post("/items/2", json={"name": "bad"})
assert resp.status_code == 422

# HTTPException paths
assert client.get("/items/99").status_code == 404
resp = client.post("/items/1", json={"name": "dup", "price": 1.0})
assert resp.status_code == 409 and resp.json()["detail"] == "exists"

# async endpoint
resp = client.get("/async-sum", params={"a": 20, "b": 22})
assert resp.status_code == 200 and resp.json() == {"total": 42}

# OpenAPI schema generation (exercises pydantic JSON-schema machinery)
schema = client.get("/openapi.json").json()
assert schema["info"]["title"] == "FastAPI"
assert "/items/{item_id}" in schema["paths"]

import fastapi  # noqa: E402

print("fastapi ok", fastapi.__version__)
