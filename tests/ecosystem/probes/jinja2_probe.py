"""Ecosystem probe: jinja2 — rendering incl. autoescape and inheritance."""

import jinja2
from jinja2 import DictLoader, Environment, Template

assert Template("{{ x + 1 }}").render(x=41) == "42"
assert Template("{% for i in xs %}{{ i }}{% endfor %}").render(xs=[1, 2, 3]) == "123"

# autoescape
env = Environment(autoescape=True)
out = env.from_string("{{ v }}").render(v="<script>")
assert out == "&lt;script&gt;", out

# filters + tests
assert Template("{{ 'abc'|upper }}").render() == "ABC"
assert Template("{{ 1 if x is odd else 0 }}").render(x=3) == "1"

# template inheritance through a loader
env = Environment(
    loader=DictLoader(
        {
            "base.html": "<title>{% block t %}default{% endblock %}</title>",
            "child.html": "{% extends 'base.html' %}{% block t %}mine{% endblock %}",
        }
    )
)
assert env.get_template("child.html").render() == "<title>mine</title>"

# macros
out = Template(
    "{% macro hi(n) %}hi {{ n }}{% endmacro %}{{ hi('there') }}"
).render()
assert out == "hi there"

print("jinja2 ok", jinja2.__version__)
