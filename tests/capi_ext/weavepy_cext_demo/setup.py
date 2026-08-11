"""setup.py for the RFC 0062 C-extension source-build fixture."""

from setuptools import Extension, setup

setup(
    packages=["weavepy_cext_demo"],
    ext_modules=[
        Extension(
            "weavepy_cext_demo._demo",
            sources=["src/demo.c"],
        )
    ],
)
