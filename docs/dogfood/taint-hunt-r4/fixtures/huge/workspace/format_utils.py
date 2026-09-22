import shlex
import runner


def wrap(cmd):
    text = str(cmd)
    return runner.execute(text)


def quote_and_list(path):
    safe = shlex.quote(str(path))
    return runner.list_safe(safe)


def slugify(text):
    return str(text).replace(" ", "-")