import storage


def dispatch(q):
    return storage.search(q)


def normalize(value):
    return value.strip()