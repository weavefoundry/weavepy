"""Ecosystem probe: pydantic v2 — model validate/dump, field validator,
and the ValidationError shape (exercises the pydantic-core Rust wheel)."""

import pydantic
from pydantic import BaseModel, Field, ValidationError, field_validator


class User(BaseModel):
    id: int
    name: str = Field(min_length=2)
    tags: list[str] = []

    @field_validator("name")
    @classmethod
    def name_not_reserved(cls, v: str) -> str:
        if v == "root":
            raise ValueError("reserved name")
        return v.title()


u = User.model_validate({"id": "7", "name": "ada", "tags": ("x", "y")})
assert u.id == 7 and u.name == "Ada" and u.tags == ["x", "y"], u

dumped = u.model_dump()
assert dumped == {"id": 7, "name": "Ada", "tags": ["x", "y"]}, dumped
assert u.model_dump_json() == '{"id":7,"name":"Ada","tags":["x","y"]}'

# round-trip
assert User.model_validate_json(u.model_dump_json()) == u

# ValidationError shape: field locations and error types
try:
    User(id="not-an-int", name="root")
except ValidationError as e:
    errs = e.errors()
    locs = {err["loc"][0] for err in errs}
    assert locs == {"id", "name"}, locs
    types = {err["type"] for err in errs}
    assert "int_parsing" in types, types
else:
    raise AssertionError("ValidationError not raised")

print("pydantic ok", pydantic.VERSION)
