import adapters


def handle(user_in):
    return adapters.prepare(user_in)


def record(key):
    adapters.wrap(key)


def show(text):
    return adapters.display(text)