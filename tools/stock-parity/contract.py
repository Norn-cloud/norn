"""DIFF-EXACT: stock millionths, gross GL cents; raw legs retained.
DIFF-REVIEW: observed fingerprints bind three reviewed divergence buckets.
DIFF-COVERAGE: every trace checkpoint is required; unsupported never passes.
ADAPTER-OBSERVATION: producers read committed rows, never trace expectations.
"""
from typing import Any, Callable, Dict, List, Literal, Optional, TypedDict

StockRow = TypedDict('StockRow', {
    'id': str, 'requestId': str, 'line': str, 'warehouse': str, 'cancelled': bool,
    'quantityDelta': str, 'valueDelta': str, 'quantity': str, 'value': str,
    'incomingRate': Optional[str], 'valuationRate': Optional[str],
    'fifo': Optional[List[List[str]]],
})
GlRow = TypedDict('GlRow', {'id': str, 'requestId': str, 'account': str,
    'currency': str, 'debit': str, 'credit': str, 'cancelled': bool})
Attempt = TypedDict('Attempt', {'attempt': int, 'requestId': str,
    'nativeId': Optional[str], 'outcome': Literal['accepted', 'refused'],
    'nativeCode': Optional[str], 'reason': Optional[str], 'completionOrder': int})
Document = TypedDict('Document', {'requestId': str, 'status': int})

Checkpoint = TypedDict('Checkpoint', {'step': int, 'attempts': List[Attempt],
    'stock': List[StockRow], 'gl': List[GlRow], 'documents': List[Document],
    'status': Literal['unsupported']}, total=False)

Case = TypedDict('Case', {'case': str, 'method': str,
    'status': Literal['complete', 'unsupported'], 'reason': Optional[str],
    'initial': Checkpoint, 'checkpoints': List[Checkpoint]}, total=False)

StockDump = TypedDict('StockDump', {
    'format': Literal['norn.stock-dump/1'], 'engine': Literal['norn', 'erpnext'],
    'versions': Dict[str, Any], 'cases': List[Case],
})


# Input has already passed the shared TypeScript parseStockTrace boundary.
Trace = Dict[str, Any]
NornAdapter = Callable[[List[Trace]], StockDump]


def unbound_norn(traces: List[Trace]) -> StockDump:
    return dict(format='norn.stock-dump/1', engine='norn', versions={}, cases=[
        dict(case=t['id'], method=t['method'], status='unsupported',
             reason='not yet: #850', checkpoints=[]) for t in traces])
