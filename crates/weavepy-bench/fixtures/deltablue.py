"""DeltaBlue constraint solver — the classic OO/polymorphism benchmark.

Compact port of the pyperformance/V8 deltablue workload: a chain of
equality/scale constraints is built, then repeatedly perturbed and
re-planned. Exercises method dispatch, attribute access, and list
traffic in realistic OO proportions.
"""

import os


class Strength:
    def __init__(self, value, name):
        self.value = value
        self.name = name

    @staticmethod
    def stronger(s1, s2):
        return s1.value < s2.value

    @staticmethod
    def weaker(s1, s2):
        return s1.value > s2.value

    @staticmethod
    def weakest_of(s1, s2):
        return s1 if Strength.weaker(s1, s2) else s2


REQUIRED = Strength(0, "required")
STRONG_PREFERRED = Strength(1, "strongPreferred")
PREFERRED = Strength(2, "preferred")
STRONG_DEFAULT = Strength(3, "strongDefault")
NORMAL = Strength(4, "normal")
WEAK_DEFAULT = Strength(5, "weakDefault")
WEAKEST = Strength(6, "weakest")


class Constraint:
    def __init__(self, strength):
        self.strength = strength

    def add_constraint(self, planner):
        self.add_to_graph()
        planner.incremental_add(self)

    def satisfy(self, mark, planner):
        self.choose_method(mark)
        if not self.is_satisfied():
            if self.strength is REQUIRED:
                raise RuntimeError("Could not satisfy a required constraint!")
            return None
        self.mark_inputs(mark)
        out = self.output()
        overridden = out.determined_by
        if overridden is not None:
            overridden.mark_unsatisfied()
        out.determined_by = self
        if not planner.add_propagate(self, mark):
            raise RuntimeError("Cycle encountered")
        out.mark = mark
        return overridden

    def destroy_constraint(self, planner):
        if self.is_satisfied():
            planner.incremental_remove(self)
        else:
            self.remove_from_graph()

    def is_input(self):
        return False


class UnaryConstraint(Constraint):
    def __init__(self, v, strength, planner):
        super().__init__(strength)
        self.my_output = v
        self.satisfied = False
        self.add_constraint(planner)

    def add_to_graph(self):
        self.my_output.add_constraint(self)
        self.satisfied = False

    def choose_method(self, mark):
        if self.my_output.mark != mark and Strength.stronger(
            self.strength, self.my_output.walk_strength
        ):
            self.satisfied = True
        else:
            self.satisfied = False

    def is_satisfied(self):
        return self.satisfied

    def mark_inputs(self, mark):
        pass

    def output(self):
        return self.my_output

    def recalculate(self):
        self.my_output.walk_strength = self.strength
        self.my_output.stay = not self.is_input()
        if self.my_output.stay:
            self.execute()

    def mark_unsatisfied(self):
        self.satisfied = False

    def inputs_known(self, mark):
        return True

    def remove_from_graph(self):
        if self.my_output is not None:
            self.my_output.remove_constraint(self)
            self.satisfied = False


class StayConstraint(UnaryConstraint):
    def execute(self):
        pass


class EditConstraint(UnaryConstraint):
    def is_input(self):
        return True

    def execute(self):
        pass


class BinaryConstraint(Constraint):
    NONE = 0
    FORWARD = 1
    BACKWARD = 2

    def __init__(self, v1, v2, strength, planner):
        super().__init__(strength)
        self.v1 = v1
        self.v2 = v2
        self.direction = BinaryConstraint.NONE
        self.add_constraint(planner)

    def choose_method(self, mark):
        if self.v1.mark == mark:
            if self.v2.mark != mark and Strength.stronger(
                self.strength, self.v2.walk_strength
            ):
                self.direction = BinaryConstraint.FORWARD
            else:
                self.direction = BinaryConstraint.NONE
        elif self.v2.mark == mark:
            if self.v1.mark != mark and Strength.stronger(
                self.strength, self.v1.walk_strength
            ):
                self.direction = BinaryConstraint.BACKWARD
            else:
                self.direction = BinaryConstraint.NONE
        elif Strength.weaker(self.v1.walk_strength, self.v2.walk_strength):
            if Strength.stronger(self.strength, self.v1.walk_strength):
                self.direction = BinaryConstraint.BACKWARD
            else:
                self.direction = BinaryConstraint.NONE
        else:
            if Strength.stronger(self.strength, self.v2.walk_strength):
                self.direction = BinaryConstraint.FORWARD
            else:
                self.direction = BinaryConstraint.NONE

    def add_to_graph(self):
        self.v1.add_constraint(self)
        self.v2.add_constraint(self)
        self.direction = BinaryConstraint.NONE

    def is_satisfied(self):
        return self.direction != BinaryConstraint.NONE

    def mark_inputs(self, mark):
        self.input().mark = mark

    def input(self):
        return self.v1 if self.direction == BinaryConstraint.FORWARD else self.v2

    def output(self):
        return self.v2 if self.direction == BinaryConstraint.FORWARD else self.v1

    def recalculate(self):
        ihn = self.input()
        out = self.output()
        out.walk_strength = Strength.weakest_of(self.strength, ihn.walk_strength)
        out.stay = ihn.stay
        if out.stay:
            self.execute()

    def mark_unsatisfied(self):
        self.direction = BinaryConstraint.NONE

    def inputs_known(self, mark):
        i = self.input()
        return i.mark == mark or i.stay or i.determined_by is None

    def remove_from_graph(self):
        if self.v1 is not None:
            self.v1.remove_constraint(self)
        if self.v2 is not None:
            self.v2.remove_constraint(self)
        self.direction = BinaryConstraint.NONE


class ScaleConstraint(BinaryConstraint):
    def __init__(self, src, scale, offset, dest, strength, planner):
        self.scale = scale
        self.offset = offset
        super().__init__(src, dest, strength, planner)

    def add_to_graph(self):
        super().add_to_graph()
        self.scale.add_constraint(self)
        self.offset.add_constraint(self)

    def remove_from_graph(self):
        super().remove_from_graph()
        if self.scale is not None:
            self.scale.remove_constraint(self)
        if self.offset is not None:
            self.offset.remove_constraint(self)

    def mark_inputs(self, mark):
        super().mark_inputs(mark)
        self.scale.mark = mark
        self.offset.mark = mark

    def execute(self):
        if self.direction == BinaryConstraint.FORWARD:
            self.v2.value = self.v1.value * self.scale.value + self.offset.value
        else:
            self.v1.value = (self.v2.value - self.offset.value) // self.scale.value

    def recalculate(self):
        ihn = self.input()
        out = self.output()
        out.walk_strength = Strength.weakest_of(self.strength, ihn.walk_strength)
        out.stay = ihn.stay and self.scale.stay and self.offset.stay
        if out.stay:
            self.execute()


class EqualityConstraint(BinaryConstraint):
    def execute(self):
        self.output().value = self.input().value


class Variable:
    def __init__(self, name, value=0):
        self.name = name
        self.value = value
        self.constraints = []
        self.determined_by = None
        self.mark = 0
        self.walk_strength = WEAKEST
        self.stay = True

    def add_constraint(self, constraint):
        self.constraints.append(constraint)

    def remove_constraint(self, constraint):
        if constraint in self.constraints:
            self.constraints.remove(constraint)
        if self.determined_by is constraint:
            self.determined_by = None


class Plan:
    def __init__(self):
        self.v = []

    def add_constraint(self, c):
        self.v.append(c)

    def execute(self):
        for c in self.v:
            c.execute()


class Planner:
    def __init__(self):
        self.current_mark = 0

    def new_mark(self):
        self.current_mark += 1
        return self.current_mark

    def incremental_add(self, constraint):
        mark = self.new_mark()
        overridden = constraint.satisfy(mark, self)
        while overridden is not None:
            overridden = overridden.satisfy(mark, self)

    def incremental_remove(self, constraint):
        out = constraint.output()
        constraint.mark_unsatisfied()
        constraint.remove_from_graph()
        unsatisfied = self.remove_propagate_from(out)
        strength = REQUIRED
        while True:
            for u in unsatisfied:
                if u.strength is strength:
                    self.incremental_add(u)
            if strength is WEAKEST:
                break
            strength = {
                REQUIRED: STRONG_PREFERRED,
                STRONG_PREFERRED: PREFERRED,
                PREFERRED: STRONG_DEFAULT,
                STRONG_DEFAULT: NORMAL,
                NORMAL: WEAK_DEFAULT,
                WEAK_DEFAULT: WEAKEST,
            }[strength]

    def add_propagate(self, c, mark):
        todo = [c]
        while todo:
            d = todo.pop()
            if d.output().mark == mark:
                self.incremental_remove(c)
                return False
            d.recalculate()
            self.add_constraints_consuming_to(d.output(), todo)
        return True

    def remove_propagate_from(self, out):
        out.determined_by = None
        out.walk_strength = WEAKEST
        out.stay = True
        unsatisfied = []
        todo = [out]
        while todo:
            v = todo.pop()
            for c in v.constraints:
                if not c.is_satisfied():
                    unsatisfied.append(c)
            determining = v.determined_by
            for c in v.constraints:
                if c is not determining and c.is_satisfied():
                    c.recalculate()
                    todo.append(c.output())
        return unsatisfied

    def add_constraints_consuming_to(self, v, coll):
        determining = v.determined_by
        for c in v.constraints:
            if c is not determining and c.is_satisfied():
                coll.append(c)

    def make_plan(self, sources):
        mark = self.new_mark()
        plan = Plan()
        todo = list(sources)
        while todo:
            c = todo.pop()
            if c.output().mark != mark and c.inputs_known(mark):
                plan.add_constraint(c)
                c.output().mark = mark
                self.add_constraints_consuming_to(c.output(), todo)
        return plan

    def extract_plan_from_constraints(self, constraints):
        sources = [c for c in constraints if c.is_input() and c.is_satisfied()]
        return self.make_plan(sources)


def chain_test(n, planner):
    prev = first = last = None
    for i in range(n + 1):
        v = Variable("v%d" % i)
        if prev is not None:
            EqualityConstraint(prev, v, REQUIRED, planner)
        if i == 0:
            first = v
        if i == n:
            last = v
        prev = v
    StayConstraint(last, STRONG_DEFAULT, planner)
    edit = EditConstraint(first, PREFERRED, planner)
    plan = planner.extract_plan_from_constraints([edit])
    for i in range(100):
        first.value = i
        plan.execute()
        if last.value != i:
            raise RuntimeError("Chain test failed")
    edit.destroy_constraint(planner)


def projection_test(n, planner):
    scale = Variable("scale", 10)
    offset = Variable("offset", 1000)
    src = dst = None
    dests = []
    for i in range(n):
        src = Variable("src%d" % i, i)
        dst = Variable("dst%d" % i, i)
        dests.append(dst)
        StayConstraint(src, NORMAL, planner)
        ScaleConstraint(src, scale, offset, dst, REQUIRED, planner)
    change(src, 17, planner)
    if dst.value != 1170:
        raise RuntimeError("Projection 1 failed")
    change(dst, 1050, planner)
    if src.value != 5:
        raise RuntimeError("Projection 2 failed")
    change(scale, 5, planner)
    for i in range(n - 1):
        if dests[i].value != i * 5 + 1000:
            raise RuntimeError("Projection 3 failed")
    change(offset, 2000, planner)
    for i in range(n - 1):
        if dests[i].value != i * 5 + 2000:
            raise RuntimeError("Projection 4 failed")


def change(v, new_value, planner):
    edit = EditConstraint(v, PREFERRED, planner)
    plan = planner.extract_plan_from_constraints([edit])
    for _ in range(10):
        v.value = new_value
        plan.execute()
    edit.destroy_constraint(planner)


def bench(n):
    for _ in range(n):
        planner = Planner()
        chain_test(50, planner)
        projection_test(50, planner)
    return 0


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "10"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
