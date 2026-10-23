"""Word frequency helpers."""


def top_words(text: str, n: int) -> list[tuple[str, int]]:
    """Return the `n` most frequent words in `text` as (word, count) pairs.

    Words are case-insensitive runs of letters, digits or apostrophes
    ("don't" is one word). Sort by count descending, then alphabetically.
    """
    raise NotImplementedError
