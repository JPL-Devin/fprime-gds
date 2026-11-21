"""Predicates: callables of one argument, evaluated in this process only (SCR-6).

The names and meanings are those of the predicates of the F Prime integration
test API, so existing tests keep working with only their import changed.
"""

from inspect import signature

from .data import ChData, EventData


class predicate:
    def __call__(self, item):
        raise NotImplementedError("This predicate did not override __call__(self, object)")

    def __str__(self):
        raise NotImplementedError("This predicate did not override __str__(self)")

    def __repr__(self):
        return str(self)


def is_predicate(pred):
    if isinstance(pred, predicate):
        return True
    if callable(pred):
        try:
            return len(signature(pred.__call__).parameters) == 1
        except (TypeError, ValueError):
            return False
    return False


def get_descriptive_string(value, pred_function):
    return f"F({value}), where F(x) evaluates\n\t {pred_function}"


class _compare(predicate):
    symbol = "?"

    def __init__(self, value):
        self.value = value

    def test(self, actual):
        raise NotImplementedError

    def __call__(self, actual):
        try:
            return bool(self.test(actual))
        except TypeError:
            return False

    def __str__(self):
        return f"x {self.symbol} {self.value}"


class less_than(_compare):
    symbol = "<"

    def test(self, actual):
        return actual < self.value


class greater_than(_compare):
    symbol = ">"

    def test(self, actual):
        return actual > self.value


class equal_to(_compare):
    symbol = "=="

    def test(self, actual):
        return actual == self.value


class not_equal_to(_compare):
    symbol = "!="

    def test(self, actual):
        return actual != self.value


class less_than_or_equal_to(_compare):
    symbol = "<="

    def test(self, actual):
        return actual <= self.value


class greater_than_or_equal_to(_compare):
    symbol = ">="

    def test(self, actual):
        return actual >= self.value


class within_range(predicate):
    def __init__(self, lower, upper):
        self.lower_limit, self.upper_limit = lower, upper

    def __call__(self, actual):
        try:
            return self.lower_limit <= actual <= self.upper_limit
        except TypeError:
            return False

    def __str__(self):
        return f"{self.lower_limit} <= x <= {self.upper_limit}"


class is_a_member_of(predicate):
    def __init__(self, collection):
        self.set = collection

    def __call__(self, item):
        return any(item == x for x in self.set)

    def __str__(self):
        return f"x ∈ {self.set}"


class is_not_a_member_of(predicate):
    def __init__(self, collection):
        self.set = collection

    def __call__(self, item):
        return all(item != x for x in self.set)

    def __str__(self):
        return f"x ∉ {self.set}"


class always_true(predicate):
    def __call__(self, obj):
        return True

    def __str__(self):
        return "True"


class invert(predicate):
    def __init__(self, pred):
        if not is_predicate(pred):
            raise TypeError("The given argument was not an instance of predicate.")
        self.pred = pred

    def __call__(self, item):
        return not self.pred(item)

    def __str__(self):
        return f"˜({self.pred})."


def _checked(pred_list):
    for pred in pred_list:
        if not is_predicate(pred):
            raise TypeError(f"The given list item, {pred}, was not an instance of predicate.")
    return list(pred_list)


class satisfies_all(predicate):
    def __init__(self, pred_list):
        self.p_list = _checked(pred_list)

    def __call__(self, item):
        return all(pred(item) for pred in self.p_list)

    def __str__(self):
        return f"∀ P ∈ A : P(x) is True, when A is {self.p_list}"


class satisfies_any(predicate):
    def __init__(self, pred_list):
        self.p_list = _checked(pred_list)

    def __call__(self, item):
        return any(pred(item) for pred in self.p_list)

    def __str__(self):
        return f"∃ P ∈ A : P(x) is True, when A is {self.p_list}"


class args_predicate(predicate):
    """Matches an argument list element by element; `None` accepts any value."""

    def __init__(self, args):
        if not isinstance(args, (list, tuple)):
            args = [args]
        self.arg_spec = [
            always_true() if arg is None else arg if is_predicate(arg) else equal_to(arg) for arg in args
        ]

    def __call__(self, actual):
        if not isinstance(actual, (list, tuple)):
            actual = [actual]
        if len(actual) != len(self.arg_spec):
            return False
        return all(spec(value) for spec, value in zip(self.arg_spec, actual))

    def __str__(self):
        return f"True IFF ∀ pi ∈ P and xi ∈ x; pi(xi) is True. Where P is {self.arg_spec}"


def _or_true(pred):
    return pred if is_predicate(pred) else always_true()


def _describe(start, parts):
    for label, pred in parts:
        if not isinstance(pred, always_true):
            start += f" and x's {label} ({pred})"
    return start


class event_predicate(predicate):
    def __init__(self, id_pred=None, args_pred=None, severity_pred=None, time_pred=None):
        self.id_pred, self.args_pred = _or_true(id_pred), _or_true(args_pred)
        self.severity_pred, self.time_pred = _or_true(severity_pred), _or_true(time_pred)

    def __call__(self, event):
        return (
            isinstance(event, EventData)
            and self.id_pred(event.get_id())
            and self.time_pred(event.get_time())
            and self.severity_pred(event.get_severity())
            and self.args_pred(event.get_args())
        )

    def __str__(self):
        return _describe(
            "True IFF: x is an EventData object",
            [
                ("id satisfies", self.id_pred),
                ("args satisfy", self.args_pred),
                ("severity satisfies", self.severity_pred),
                ("time satisfies", self.time_pred),
            ],
        )


class telemetry_predicate(predicate):
    def __init__(self, id_pred=None, value_pred=None, time_pred=None):
        self.id_pred, self.value_pred, self.time_pred = _or_true(id_pred), _or_true(value_pred), _or_true(time_pred)

    def __call__(self, telemetry):
        return (
            isinstance(telemetry, ChData)
            and self.id_pred(telemetry.get_id())
            and self.value_pred(telemetry.get_val())
            and self.time_pred(telemetry.get_time())
        )

    def __str__(self):
        return _describe(
            "True IFF: x is a ChData object",
            [("id satisfies", self.id_pred), ("value satisfies", self.value_pred), ("time satisfies", self.time_pred)],
        )
