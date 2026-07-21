"""Ecosystem probe: click — CLI invocation via CliRunner."""

import click
from click.testing import CliRunner


@click.command()
@click.option("--count", default=1, help="Number of greetings.")
@click.argument("name")
def hello(count, name):
    for _ in range(count):
        click.echo(f"Hello {name}!")


runner = CliRunner()
result = runner.invoke(hello, ["--count", "2", "world"])
assert result.exit_code == 0, result.output
assert result.output == "Hello world!\nHello world!\n", repr(result.output)

# bad option → usage error, non-zero exit
result = runner.invoke(hello, ["--count", "x", "world"])
assert result.exit_code != 0

# group dispatch
@click.group()
def cli():
    pass


@cli.command()
def sub():
    click.echo("sub ran")


result = runner.invoke(cli, ["sub"])
assert result.exit_code == 0 and "sub ran" in result.output

print("click ok", click.__version__)
