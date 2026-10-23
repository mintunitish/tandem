"""Renders a sales table in three formats. Output must stay byte-identical."""

ROWS = [("north", 1200, 0.15), ("south", 830, -0.02), ("east", 4310, 0.4), ("west", 77, 0.0)]


def render_text(rows):
    out = []
    for region, revenue, growth in rows:
        name = region.capitalize().ljust(8)
        money = f"${revenue:,}".rjust(8)
        pct = f"{growth * 100:+.1f}%".rjust(7)
        out.append(f"{name}{money}{pct}")
    return "\n".join(out)


def render_markdown(rows):
    out = ["| Region | Revenue | Growth |", "|---|---|---|"]
    for region, revenue, growth in rows:
        name = region.capitalize().ljust(8)
        money = f"${revenue:,}".rjust(8)
        pct = f"{growth * 100:+.1f}%".rjust(7)
        out.append(f"| {name.strip()} | {money.strip()} | {pct.strip()} |")
    return "\n".join(out)


def render_csv(rows):
    out = ["region,revenue,growth"]
    for region, revenue, growth in rows:
        name = region.capitalize().ljust(8)
        money = f"${revenue:,}".rjust(8)
        pct = f"{growth * 100:+.1f}%".rjust(7)
        out.append(f"{name.strip()},\"{money.strip()}\",{pct.strip()}")
    return "\n".join(out)


if __name__ == "__main__":
    print(render_text(ROWS))
    print()
    print(render_markdown(ROWS))
    print()
    print(render_csv(ROWS))
