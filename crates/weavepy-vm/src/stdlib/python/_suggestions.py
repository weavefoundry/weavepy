"""WeavePy `_suggestions`: CPython's `Modules/_suggestions.c`.

`traceback` (NameError/AttributeError "Did you mean" hints and the 3.14
keyword-typo probe for SyntaxErrors) prefers this module's
`_generate_suggestions` over its own pure-Python fallback. The algorithm
is `Python/suggestions.c`: a bounded Levenshtein distance where a
case-only change costs 1, any other edit 2, and a candidate qualifies
when no more than a third of the involved characters need changing.
"""

import sys

_MAX_CANDIDATE_ITEMS = 750
_MAX_STRING_SIZE = 40
_MOVE_COST = 2
_CASE_COST = 1


def _substitution_cost(ch_a, ch_b):
    if ch_a == ch_b:
        return 0
    if ch_a.lower() == ch_b.lower():
        return _CASE_COST
    return _MOVE_COST


def _levenshtein_distance(a, b, max_cost):
    if a == b:
        return 0
    # Trim away common affixes.
    pre = 0
    while a[pre:] and b[pre:] and a[pre] == b[pre]:
        pre += 1
    a = a[pre:]
    b = b[pre:]
    post = 0
    while a[:post or None] and b[:post or None] and a[post - 1] == b[post - 1]:
        post -= 1
    a = a[:post or None]
    b = b[:post or None]
    if not a or not b:
        return _MOVE_COST * (len(a) + len(b))
    if len(a) > _MAX_STRING_SIZE or len(b) > _MAX_STRING_SIZE:
        return max_cost + 1
    # Prefer the shorter buffer.
    if len(b) < len(a):
        a, b = b, a
    # Quick fail when a match is impossible.
    if (len(b) - len(a)) * _MOVE_COST > max_cost:
        return max_cost + 1
    row = list(range(_MOVE_COST, _MOVE_COST * (len(a) + 1), _MOVE_COST))
    result = 0
    for bindex in range(len(b)):
        bchar = b[bindex]
        distance = result = bindex * _MOVE_COST
        minimum = sys.maxsize
        for index in range(len(a)):
            substitute = distance + _substitution_cost(bchar, a[index])
            distance = row[index]
            insert_delete = min(result, distance) + _MOVE_COST
            result = min(insert_delete, substitute)
            row[index] = result
            if result < minimum:
                minimum = result
        if minimum > max_cost:
            return max_cost + 1
    return result


def _generate_suggestions(candidates, item):
    """Return the candidate closest to `item`, or None."""
    if type(candidates) is not list:
        raise TypeError("candidates must be a list")
    for elem in candidates:
        if not isinstance(elem, str):
            raise TypeError("all elements in 'candidates' must be strings")
    if not isinstance(item, str):
        raise TypeError(
            f"_generate_suggestions() argument 2 must be str, not {type(item).__name__}"
        )
    if len(candidates) > _MAX_CANDIDATE_ITEMS:
        return None
    wrong_name_len = len(item)
    if wrong_name_len > _MAX_STRING_SIZE:
        return None
    best_distance = sys.maxsize
    suggestion = None
    for possible_name in candidates:
        if possible_name == item:
            # A missing attribute is "found". Don't suggest it (GH-88821).
            continue
        # No more than 1/3 of the involved characters should need changed.
        max_distance = (len(possible_name) + wrong_name_len + 3) * _MOVE_COST // 6
        # Don't take matches we've already beaten.
        max_distance = min(max_distance, best_distance - 1)
        current_distance = _levenshtein_distance(item, possible_name, max_distance)
        if current_distance > max_distance:
            continue
        if not suggestion or current_distance < best_distance:
            suggestion = possible_name
            best_distance = current_distance
    return suggestion
