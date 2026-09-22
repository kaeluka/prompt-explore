import repo


def prepare(user_in):
    payload = {"v": user_in}
    return repo.lookup(payload["v"])


def wrap(key):
    blob = key.encode()
    return repo.write(blob.decode())


def display(text):
    label = str(text)
    return repo.render(label)