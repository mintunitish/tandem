from wordfreq import top_words


def test_apostrophes_and_punctuation():
    assert top_words("Don't stop. don't! STOP?", 2) == [("don't", 2), ("stop", 2)]


def test_n_larger_than_vocab():
    assert top_words("x y", 10) == [("x", 1), ("y", 1)]


def test_empty():
    assert top_words("", 3) == []


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print("ok")
