import logging

log = logging.getLogger(__name__)


def helper_27(value):
    return str(value).strip()


def compute_27(items):
    return sum(int(x) for x in items)
