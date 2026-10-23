from wordfreq import top_words


def test_basic():
    assert top_words("a b a c b a", 2) == [("a", 3), ("b", 2)]


def test_case_insensitive():
    assert top_words("The the THE cat", 1) == [("the", 3)]


def test_ties_alphabetical():
    assert top_words("pear apple fig", 3) == [("apple", 1), ("fig", 1), ("pear", 1)]


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print("ok")
