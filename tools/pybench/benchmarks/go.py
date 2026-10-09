"""Random playouts of Go on a small board: object graphs, union-find,
lists of objects, and many tiny methods (after pyperformance's `go`)."""

import random

WORK = 60
SIZE = 9
EMPTY, WHITE, BLACK = 0, 1, 2
SHOW = {EMPTY: ".", WHITE: "o", BLACK: "x"}


class Square:
    def __init__(self, board, pos):
        self.board = board
        self.pos = pos
        self.timestamp = board.time
        self.removestamp = board.time
        self.zobrist_strings = [board.rng.randrange(1 << 62) for _ in range(3)]
        self.neighbours = []
        self.color = EMPTY
        self.reference = self
        self.ledges = 0
        self.used = False
        self.temp_ledges = 0

    def set_neighbours(self):
        x, y = self.pos % SIZE, self.pos // SIZE
        self.neighbours = []
        for dx, dy in ((-1, 0), (1, 0), (0, -1), (0, 1)):
            nx, ny = x + dx, y + dy
            if 0 <= nx < SIZE and 0 <= ny < SIZE:
                self.neighbours.append(self.board.squares[nx + ny * SIZE])

    def move(self, color):
        self.board.time += 1
        self.board.zobrist.update(self, color)
        self.color = color
        self.reference = self
        self.ledges = 0
        self.used = True
        for neighbour in self.neighbours:
            neighcolor = neighbour.color
            if neighcolor == EMPTY:
                self.ledges += 1
            else:
                neighbour_ref = neighbour.find(update=True)
                if neighcolor == color:
                    if neighbour_ref.reference.pos != self.pos:
                        self.ledges += neighbour_ref.ledges
                        neighbour_ref.reference = self
                    self.ledges -= 1
                else:
                    neighbour_ref.ledges -= 1
                    if neighbour_ref.ledges == 0:
                        neighbour.remove(neighbour_ref)
        self.board.zobrist.add()

    def remove(self, reference, update=True):
        self.board.zobrist.update(self, EMPTY)
        self.removestamp = self.board.time
        if update:
            self.color = EMPTY
            self.board.emptyset.add(self.pos)
        for neighbour in self.neighbours:
            if neighbour.color != EMPTY and neighbour.removestamp != self.board.time:
                neighbour_ref = neighbour.find(update)
                if neighbour_ref.pos == reference.pos:
                    neighbour.remove(reference, update)
                elif update:
                    neighbour_ref.ledges += 1

    def find(self, update=False):
        reference = self.reference
        if reference.pos != self.pos:
            reference = reference.find(update)
            if update:
                self.reference = reference
        return reference


class EmptySet:
    def __init__(self, board):
        self.board = board
        self.empties = list(range(SIZE * SIZE))
        self.empty_pos = list(range(SIZE * SIZE))

    def random_choice(self):
        choices = len(self.empties)
        while choices:
            i = int(self.board.rng.random() * choices)
            pos = self.empties[i]
            if self.board.useful(pos):
                return pos
            choices -= 1
            self.set(i, self.empties[choices])
            self.set(choices, pos)
        return -1

    def add(self, pos):
        self.empty_pos[pos] = len(self.empties)
        self.empties.append(pos)

    def remove(self, pos):
        self.set(self.empty_pos[pos], self.empties[len(self.empties) - 1])
        self.empties.pop()

    def set(self, i, pos):
        self.empties[i] = pos
        self.empty_pos[pos] = i


class ZobristHash:
    def __init__(self, board):
        self.board = board
        self.hash_set = set()
        self.hash = 0
        for square in board.squares:
            self.hash ^= square.zobrist_strings[EMPTY]
        self.hash_set.clear()
        self.hash_set.add(self.hash)

    def update(self, square, color):
        self.hash ^= square.zobrist_strings[square.color]
        self.hash ^= square.zobrist_strings[color]

    def add(self):
        self.hash_set.add(self.hash)

    def dupe(self):
        return self.hash in self.hash_set


class Board:
    def __init__(self, rng):
        self.rng = rng
        self.time = 0
        self.squares = [Square(self, pos) for pos in range(SIZE * SIZE)]
        for square in self.squares:
            square.set_neighbours()
        self.reset()

    def reset(self):
        for square in self.squares:
            square.color = EMPTY
            square.used = False
        self.emptyset = EmptySet(self)
        self.zobrist = ZobristHash(self)
        self.color = BLACK
        self.finished = False
        self.lastmove = -2
        self.history = []
        self.white_dead = 0
        self.black_dead = 0

    def move(self, pos):
        square = self.squares[pos]
        if pos != -1:
            square.move(self.color)
            self.emptyset.remove(square.pos)
        elif self.lastmove == -1:
            self.finished = True
        self.color = BLACK if self.color == WHITE else WHITE
        self.lastmove = pos
        self.history.append(pos)

    def random_move(self):
        return self.emptyset.random_choice()

    def useful_fast(self, square):
        if not square.used:
            for neighbour in square.neighbours:
                if neighbour.color == EMPTY:
                    return True
        return False

    def useful(self, pos):
        square = self.squares[pos]
        if self.useful_fast(square):
            return True
        old_hash = self.zobrist.hash
        self.zobrist.update(square, self.color)
        empties = opps = weak_opps = neighs = weak_neighs = 0
        for neighbour in square.neighbours:
            neighcolor = neighbour.color
            if neighcolor == EMPTY:
                empties += 1
                continue
            neighbour_ref = neighbour.find()
            if neighbour_ref.timestamp != self.time:
                if neighcolor == self.color:
                    neighs += 1
                else:
                    opps += 1
                neighbour_ref.timestamp = self.time
                neighbour_ref.temp_ledges = neighbour_ref.ledges
            neighbour_ref.temp_ledges -= 1
            if neighbour_ref.temp_ledges == 0:
                if neighcolor == self.color:
                    weak_neighs += 1
                else:
                    weak_opps += 1
                    neighbour_ref.remove(neighbour_ref, update=False)
        dupe = self.zobrist.dupe()
        self.zobrist.hash = old_hash
        strong_neighs = neighs - weak_neighs
        strong_opps = opps - weak_opps
        return not dupe and (empties or weak_opps or (strong_neighs and (strong_opps or weak_neighs)))

    def score(self, color):
        count = self.white_dead if color == WHITE else self.black_dead
        for square in self.squares:
            squarecolor = square.color
            if squarecolor == color:
                count += 1
            elif squarecolor == EMPTY:
                surround = 0
                for neighbour in square.neighbours:
                    if neighbour.color == color:
                        surround += 1
                if surround == len(square.neighbours):
                    count += 1
        return count


def bench(n):
    rng = random.Random(42)
    board = Board(rng)
    total = 0
    for _ in range(n):
        board.reset()
        for _ in range(SIZE * SIZE * 2):
            if board.finished:
                break
            board.move(board.random_move())
        total += board.score(WHITE) - board.score(BLACK) + len(board.history)
    return total
