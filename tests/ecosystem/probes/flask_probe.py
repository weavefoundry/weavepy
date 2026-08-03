"""Ecosystem probe: flask — route + test-client request/response cycle,
JSON endpoint, and a session cookie round-trip."""

import flask
from flask import Flask, jsonify, request, session

app = Flask(__name__)
app.secret_key = "probe-secret"


@app.route("/")
def index():
    return "hello world"


@app.route("/add")
def add():
    a = int(request.args["a"])
    b = int(request.args["b"])
    return jsonify(total=a + b)


@app.route("/login", methods=["POST"])
def login():
    session["user"] = request.form["user"]
    return "ok"


@app.route("/whoami")
def whoami():
    return session.get("user", "anonymous")


client = app.test_client()

r = client.get("/")
assert r.status_code == 200, r.status_code
assert r.data == b"hello world", r.data

r = client.get("/add?a=20&b=22")
assert r.status_code == 200
assert r.get_json() == {"total": 42}, r.get_json()

# session cookie round-trip: /login sets it, /whoami reads it back
assert client.get("/whoami").data == b"anonymous"
r = client.post("/login", data={"user": "weave"})
assert r.status_code == 200
assert client.get("/whoami").data == b"weave"

# 404 path
assert client.get("/missing").status_code == 404

print("flask ok", flask.__version__)
