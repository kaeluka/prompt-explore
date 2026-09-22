import logging

log = logging.getLogger(__name__)


def helper_21(value):
    return str(value).strip()


def compute_21(items):
    return sum(int(x) for x in items)
