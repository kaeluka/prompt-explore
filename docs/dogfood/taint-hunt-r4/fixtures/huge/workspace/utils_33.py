import logging

log = logging.getLogger(__name__)


def helper_33(value):
    return str(value).strip()


def compute_33(items):
    return sum(int(x) for x in items)
