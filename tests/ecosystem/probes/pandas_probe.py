"""Ecosystem probe: pandas capstone (RFC 0058 WS8) — DataFrame
construction, dtype/aggregation behaviour, groupby, merge, datetime
indexing, and a CSV round-trip through io.StringIO."""

import io

import pandas as pd

# --- construction + dtypes ---------------------------------------------------
df = pd.DataFrame(
    {
        "name": ["python", "rust", "go", "zig"],
        "year": [1991, 2015, 2009, 2016],
        "typed": [False, True, True, True],
    }
)
assert df.shape == (4, 3), df.shape
assert str(df["year"].dtype) == "int64", df["year"].dtype
assert df["year"].sum() == 8031

# --- selection + boolean masks ----------------------------------------------
modern = df[df["year"] >= 2000]
assert sorted(modern["name"]) == ["go", "rust", "zig"]
assert df.loc[df["name"] == "rust", "year"].item() == 2015
assert df.iloc[0]["name"] == "python"

# --- groupby / aggregation ---------------------------------------------------
g = df.groupby("typed")["year"].agg(["count", "max"])
assert g.loc[True, "count"] == 3 and g.loc[True, "max"] == 2016, g
assert g.loc[False, "max"] == 1991

# --- merge --------------------------------------------------------------------
stars = pd.DataFrame(
    {"name": ["python", "rust", "zig"], "stars": [52, 93, 30]}
)
joined = df.merge(stars, on="name", how="left")
assert joined.shape == (4, 4)
assert joined.loc[joined["name"] == "go", "stars"].isna().all()
assert joined.loc[joined["name"] == "rust", "stars"].item() == 93

# --- datetime index + resample -----------------------------------------------
idx = pd.date_range("2024-01-01", periods=6, freq="D")
ts = pd.Series(range(6), index=idx)
weekly = ts.resample("2D").sum()
assert weekly.tolist() == [1, 5, 9], weekly.tolist()
assert ts["2024-01-03":"2024-01-05"].sum() == 2 + 3 + 4

# --- CSV round-trip -----------------------------------------------------------
buf = io.StringIO()
df.to_csv(buf, index=False)
buf.seek(0)
back = pd.read_csv(buf)
assert back.equals(df), back

# --- apply + vectorized string ops --------------------------------------------
assert df["name"].str.upper().tolist() == ["PYTHON", "RUST", "GO", "ZIG"]
assert df["year"].apply(lambda y: y // 10 * 10).tolist() == [1990, 2010, 2000, 2010]

print("pandas ok", pd.__version__)
