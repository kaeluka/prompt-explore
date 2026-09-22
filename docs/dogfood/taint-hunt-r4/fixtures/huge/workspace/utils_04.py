import logging

log = logging.getLogger(__name__)


def helper_04(value):
    return str(value).strip()


def compute_04(items):
    return sum(int(x) for x in items)
