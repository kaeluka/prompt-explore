import logging

log = logging.getLogger(__name__)


def helper_46(value):
    return str(value).strip()


def compute_46(items):
    return sum(int(x) for x in items)
