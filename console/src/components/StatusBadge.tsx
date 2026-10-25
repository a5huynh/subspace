import type { WorkerStatus } from '../api/generated/WorkerStatus'

export function StatusBadge({ status }: { status: WorkerStatus | null | undefined }) {
  if (!status) return null
  const label = status.state === 'error' ? `error: ${status.message}` : status.state
  return (
    <span className={`status status-${status.state}`} title={label}>
      {status.state}
    </span>
  )
}
