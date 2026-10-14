from utils.templates import expand, split_args


def test_arguments_fill_in_with_defaults_and_quoting():
    body = "Review. Focus on ${1:-correctness}. Rest: ${@:2}. All: $@."
    assert expand(body, split_args('"API compat" a b')) == "Review. Focus on API compat. Rest: a b. All: API compat a b."
    assert expand(body, []) == "Review. Focus on correctness. Rest: . All: ."
    assert expand("x $1 y $ARGUMENTS", ["a", "b"]) == "x a y a b"
