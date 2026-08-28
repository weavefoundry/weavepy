"""Ecosystem probe: scikit-learn (RFC 0075 WS9) — the next matrix rung
over the scipy/numpy stack, and the first row exercising joblib/loky's
spawn-with-memmap process-pool model.

Legs: LogisticRegression fit/predict on synthetic data, a
RandomForestClassifier with `n_jobs=2` (the loky process-pool leg),
Pipeline + GridSearchCV (2x2 grid), and a joblib.Memory cache
round-trip."""

import tempfile

import numpy as np

# --- synthetic data ----------------------------------------------------------
from sklearn.datasets import make_classification

X, y = make_classification(
    n_samples=400,
    n_features=8,
    n_informative=5,
    random_state=0,
)

# --- LogisticRegression fit/predict -------------------------------------------
from sklearn.linear_model import LogisticRegression
from sklearn.model_selection import train_test_split

X_tr, X_te, y_tr, y_te = train_test_split(X, y, test_size=0.25, random_state=0)
logit = LogisticRegression(max_iter=1000).fit(X_tr, y_tr)
acc = logit.score(X_te, y_te)
assert acc > 0.7, f"LogisticRegression accuracy {acc}"
proba = logit.predict_proba(X_te[:5])
assert proba.shape == (5, 2)
assert np.allclose(proba.sum(axis=1), 1.0)

# --- RandomForest with n_jobs=2: joblib/loky process pool ---------------------
from sklearn.ensemble import RandomForestClassifier

forest = RandomForestClassifier(
    n_estimators=20, n_jobs=2, random_state=0
).fit(X_tr, y_tr)
facc = forest.score(X_te, y_te)
assert facc > 0.7, f"RandomForestClassifier accuracy {facc}"

# --- Pipeline + GridSearchCV (2x2) --------------------------------------------
from sklearn.model_selection import GridSearchCV
from sklearn.pipeline import Pipeline
from sklearn.preprocessing import StandardScaler

pipe = Pipeline(
    [("scale", StandardScaler()), ("clf", LogisticRegression(max_iter=1000))]
)
grid = GridSearchCV(
    pipe,
    {"clf__C": [0.1, 1.0], "scale__with_mean": [True, False]},
    cv=3,
)
grid.fit(X_tr, y_tr)
assert grid.best_score_ > 0.7, grid.best_score_
assert grid.best_params_["clf__C"] in (0.1, 1.0)

# --- joblib.Memory cache round-trip --------------------------------------------
from joblib import Memory

cache_dir = tempfile.mkdtemp(prefix="weavepy_sklearn_cache_")
memory = Memory(cache_dir, verbose=0)
calls = []


@memory.cache
def expensive(a, b):
    calls.append((a, b))
    return a @ b


m1 = expensive(X_tr, X_tr.T)
m2 = expensive(X_tr, X_tr.T)  # served from the on-disk cache
assert np.allclose(m1, m2)
assert len(calls) == 1, f"Memory.cache re-computed: {len(calls)} calls"

import sklearn

print("scikit-learn ok", sklearn.__version__)
