function stringField(record: Record<string, unknown>, key: string): string | undefined {
  const value = record[key];
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

export function toUiError(reason: unknown): string {
  if (typeof reason === "string") return reason.trim() || "发生未知错误";
  if (reason === null || reason === undefined) return "发生未知错误";

  if (typeof reason === "object") {
    const record = reason as Record<string, unknown>;
    const message = stringField(record, "message") ?? (reason instanceof Error ? reason.name : undefined);
    const code = stringField(record, "errorCode") ?? stringField(record, "code");
    const source = stringField(record, "source");
    const assetId = stringField(record, "assetId");
    const rootId = stringField(record, "rootId");
    const jobId = stringField(record, "jobId");
    const details = [
      message,
      code ? `代码：${code}` : undefined,
      source ? `来源：${source}` : undefined,
      assetId && !message?.includes(assetId) ? `资产：${assetId}` : undefined,
      rootId && !message?.includes(rootId) ? `目录：${rootId}` : undefined,
      jobId && !message?.includes(jobId) ? `任务：${jobId}` : undefined,
    ].filter((value): value is string => Boolean(value));
    if (details.length) return details.join(" · ");

    try {
      const serialized = JSON.stringify(reason);
      if (serialized && serialized !== "{}") return serialized;
    } catch {
      // Fall through to a generic message for cyclic or otherwise unserializable values.
    }
    return "发生未知错误";
  }

  return String(reason);
}
