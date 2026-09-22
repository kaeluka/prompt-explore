import logging

log = logging.getLogger(__name__)


def helper_14(value):
    return str(value).strip()


def compute_14(items):
    return sum(int(x) for x in items)
