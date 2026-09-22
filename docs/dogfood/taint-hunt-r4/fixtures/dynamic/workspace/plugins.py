import engine

OPS = {"like": engine.by_like, "exact": engine.by_exact}


def handle(q):
    fn = OPS["like"]
    return fn(q)


def handle_exact(term):
    fn = OPS["exact"]
    return fn(term)