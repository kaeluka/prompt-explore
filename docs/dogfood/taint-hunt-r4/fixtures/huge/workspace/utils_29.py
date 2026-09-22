import logging

log = logging.getLogger(__name__)


def helper_29(value):
    return str(value).strip()


def compute_29(items):
    return sum(int(x) for x in items)
