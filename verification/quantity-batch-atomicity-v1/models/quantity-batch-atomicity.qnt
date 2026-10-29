module quantity_batch_atomicity {
  var ownerVersion: int
  var postingCount: int
  var batchCommitted: bool
  var batchInput: int
  var batchStartVersion: int
  var batchSize: int
  var batchResultVersion: int
  var publishedBatchChildren: int

  action init = all {
    ownerVersion' = 0,
    postingCount' = 0,
    batchCommitted' = false,
    batchInput' = 0,
    batchStartVersion' = 0,
    batchSize' = 0,
    batchResultVersion' = 0,
    publishedBatchChildren' = 0,
  }

  action commitBatch(input: int, batchLength: int): bool = all {
    not(batchCommitted),
    input == 1 or input == 2,
    batchLength >= 1 and batchLength <= 3,
    ownerVersion' = ownerVersion + batchLength,
    postingCount' = postingCount + batchLength,
    batchCommitted' = true,
    batchInput' = input,
    batchStartVersion' = ownerVersion,
    batchSize' = batchLength,
    batchResultVersion' = ownerVersion + batchLength,
    publishedBatchChildren' = batchLength,
  }

  action exactReplay(input: int): bool = all {
    batchCommitted,
    input == batchInput,
    ownerVersion' = ownerVersion,
    postingCount' = postingCount,
    batchCommitted' = batchCommitted,
    batchInput' = batchInput,
    batchStartVersion' = batchStartVersion,
    batchSize' = batchSize,
    batchResultVersion' = batchResultVersion,
    publishedBatchChildren' = publishedBatchChildren,
  }

  action independentPosting: bool = all {
    ownerVersion' = ownerVersion + 1,
    postingCount' = postingCount + 1,
    batchCommitted' = batchCommitted,
    batchInput' = batchInput,
    batchStartVersion' = batchStartVersion,
    batchSize' = batchSize,
    batchResultVersion' = batchResultVersion,
    publishedBatchChildren' = publishedBatchChildren,
  }

  action rejectedOrFailedBatch: bool = all {
    ownerVersion' = ownerVersion,
    postingCount' = postingCount,
    batchCommitted' = batchCommitted,
    batchInput' = batchInput,
    batchStartVersion' = batchStartVersion,
    batchSize' = batchSize,
    batchResultVersion' = batchResultVersion,
    publishedBatchChildren' = publishedBatchChildren,
  }

  action step = nondet input = 1.to(2).oneOf()
    nondet batchLength = 1.to(3).oneOf()
    any {
      commitBatch(input, batchLength),
      exactReplay(input),
      independentPosting,
      rejectedOrFailedBatch,
    }

  def typeOk = all {
    ownerVersion >= 0,
    postingCount >= 0,
    batchInput >= 0 and batchInput <= 2,
    batchStartVersion >= 0,
    batchSize >= 0 and batchSize <= 3,
    batchResultVersion >= 0,
    publishedBatchChildren >= 0 and publishedBatchChildren <= 3,
  }

  def sequentialVersions = postingCount == ownerVersion

  def batchAllOrNone =
    if (batchCommitted)
      all {
        batchSize >= 1,
        publishedBatchChildren == batchSize,
        batchResultVersion == batchStartVersion + batchSize,
        ownerVersion >= batchResultVersion,
      }
    else
      all {
        batchSize == 0,
        publishedBatchChildren == 0,
      }

  def stableReceipt =
    if (batchCommitted)
      all {
        batchInput == 1 or batchInput == 2,
        batchResultVersion > batchStartVersion,
      }
    else batchInput == 0
}
