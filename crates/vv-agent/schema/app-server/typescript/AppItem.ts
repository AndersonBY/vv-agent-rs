export type AppItem = { id: string, runEventId: string, type: AppItemKind, status: AppItemStatus, createdAtMs: bigint, completedAtMs?: bigint | null, content?: JsonValue | null, };;
