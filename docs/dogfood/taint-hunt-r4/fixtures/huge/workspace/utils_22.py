import logging

log = logging.getLogger(__name__)


def helper_22(value):
    return str(value).strip()


def compute_22(items):
    return sum(int(x) for x in items)
