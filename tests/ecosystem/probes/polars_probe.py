"""Ecosystem probe: polars (RFC 0076 WS5) — the matrix's first large
PyO3/abi3 Rust-native consumer, the Rust analogue of what grpcio was
for C++. Exercises DataFrame construction from Python values,
`group_by().agg()`, the lazy-frame optimizer through `collect()`, a
join, a parquet round-trip (the abi3 buffer + `PyBytes` surfaces),
and `map_elements` with a Python callable — the Rust→Python re-entry
leg."""

import faulthandler
import io
import sys
import tempfile

faulthandler.enable()


def stage(name: str) -> None:
    print(f"[stage] {name}", file=sys.stderr, flush=True)


stage("import")
import polars as pl

# --- construction + basic ops -----------------------------------------------
stage("construct")
df = pl.DataFrame(
    {
        "city": ["oslo", "lima", "oslo", "pune", "lima", "oslo"],
        "temp": [-3.0, 22.5, -7.5, 31.0, 19.5, 0.0],
        "reading": [1, 2, 3, 4, 5, 6],
    }
)
assert df.shape == (6, 3), df.shape
assert df["temp"].dtype == pl.Float64, df["temp"].dtype

# --- group_by().agg() ---------------------------------------------------------
stage("group_by")
agg = (
    df.group_by("city")
    .agg(
        pl.col("temp").mean().alias("mean_temp"),
        pl.len().alias("n"),
    )
    .sort("city")
)
rows = {r["city"]: (r["mean_temp"], r["n"]) for r in agg.iter_rows(named=True)}
assert rows == {
    "lima": (21.0, 2),
    "oslo": (-3.5, 3),
    "pune": (31.0, 1),
}, rows

# --- lazy frame through the optimizer ----------------------------------------
stage("lazy-collect")
lazy = (
    df.lazy()
    .filter(pl.col("temp") > 0.0)
    .with_columns((pl.col("temp") * 2).alias("double"))
    .select("city", "double")
)
collected = lazy.collect()
assert collected.height == 3, collected
assert set(collected["city"].to_list()) == {"lima", "pune"}, collected

# --- join ----------------------------------------------------------------------
stage("join")
countries = pl.DataFrame(
    {"city": ["oslo", "lima", "pune"], "country": ["NO", "PE", "IN"]}
)
joined = df.join(countries, on="city", how="left").sort("reading")
assert joined["country"].to_list() == ["NO", "PE", "NO", "IN", "PE", "NO"], (
    joined["country"].to_list()
)

# --- parquet round-trip (buffer protocol + PyBytes over abi3) ------------------
stage("parquet")
buf = io.BytesIO()
df.write_parquet(buf)
buf.seek(0)
back = pl.read_parquet(buf)
assert back.equals(df), back
with tempfile.NamedTemporaryFile(suffix=".parquet") as f:
    df.write_parquet(f.name)
    from_file = pl.read_parquet(f.name)
assert from_file.equals(df), from_file

# --- map_elements: the Rust→Python re-entry leg --------------------------------
stage("map_elements")
mapped = df.select(
    pl.col("reading")
    .map_elements(lambda x: x * x + 1, return_dtype=pl.Int64)
    .alias("sq")
)
assert mapped["sq"].to_list() == [2, 5, 10, 17, 26, 37], mapped["sq"].to_list()

stage("done")
print("polars ok", pl.__version__)
