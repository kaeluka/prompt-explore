import logging

log = logging.getLogger(__name__)


def helper_45(value):
    return str(value).strip()


def compute_45(items):
    return sum(int(x) for x in items)
