"""A runner module that prints when imported, like a chatty dependency would."""

print("imported with a top-level print")


def run(task, ctx):
    return {"status": "answered", "answer": "quiet now"}
