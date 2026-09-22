import logging

log = logging.getLogger(__name__)


def helper_34(value):
    return str(value).strip()


def compute_34(items):
    return sum(int(x) for x in items)
