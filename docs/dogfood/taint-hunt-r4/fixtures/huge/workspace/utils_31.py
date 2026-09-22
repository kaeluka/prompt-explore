import logging

log = logging.getLogger(__name__)


def helper_31(value):
    return str(value).strip()


def compute_31(items):
    return sum(int(x) for x in items)
