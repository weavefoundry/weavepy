"""Constraint-propagation sudoku solver: dicts of strings, sets, recursion."""

WORK = 6

DIGITS = "123456789"
ROWS = "ABCDEFGHI"
COLS = DIGITS


def cross(a, b):
    return [x + y for x in a for y in b]


SQUARES = cross(ROWS, COLS)
UNITLIST = ([cross(ROWS, c) for c in COLS] + [cross(r, COLS) for r in ROWS]
            + [cross(rs, cs) for rs in ("ABC", "DEF", "GHI") for cs in ("123", "456", "789")])
UNITS = {s: [u for u in UNITLIST if s in u] for s in SQUARES}
PEERS = {s: set(sum(UNITS[s], [])) - {s} for s in SQUARES}

PUZZLES = [
    "4.....8.5.3..........7......2.....6.....8.4......1.......6.3.7.5..2.....1.4......",
    "52...6.........7.13...........4..8..6......5...........418.........3..2...87.....",
    "6.....8.3.4.7.................5.4.7.3..2.....1.6.......2.....5.....8.6......1....",
    "48.3............71.2.......7.5....6....2..8.............1.76...3.....4......5....",
    "....14....3....2...7..........9...3.6.1.............8.2.....1.4....5.6.....7.8...",
    "......52..8.4......3...9...5.1...6..2..7........3.....6...1..........7.4.......3.",
]


def assign(values, s, d):
    other = values[s].replace(d, "")
    if all(eliminate(values, s, d2) for d2 in other):
        return values
    return False


def eliminate(values, s, d):
    if d not in values[s]:
        return values
    values[s] = values[s].replace(d, "")
    if len(values[s]) == 0:
        return False
    if len(values[s]) == 1:
        d2 = values[s]
        if not all(eliminate(values, s2, d2) for s2 in PEERS[s]):
            return False
    for u in UNITS[s]:
        dplaces = [s for s in u if d in values[s]]
        if len(dplaces) == 0:
            return False
        if len(dplaces) == 1 and not assign(values, dplaces[0], d):
            return False
    return values


def parse_grid(grid):
    values = {s: DIGITS for s in SQUARES}
    for s, d in zip(SQUARES, grid):
        if d in DIGITS and not assign(values, s, d):
            return False
    return values


def search(values):
    if values is False:
        return False
    if all(len(values[s]) == 1 for s in SQUARES):
        return values
    _, s = min((len(values[s]), s) for s in SQUARES if len(values[s]) > 1)
    for d in values[s]:
        result = search(assign(values.copy(), s, d))
        if result:
            return result
    return False


def bench(n):
    out = []
    for p in PUZZLES[:n]:
        v = search(parse_grid(p))
        out.append("".join(v[s] for s in SQUARES))
    return "".join(o[:9] for o in out)
