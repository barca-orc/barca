/**
 * The space of the rows a `useTableWindow` table leaves out. Renders nothing for no space, so
 * a table short enough to render whole has only its own rows.
 */
export function TableSpacer({ height, columns }: { height: number; columns: number }) {
  if (height <= 0) return null
  return (
    <tr className="barca-table-spacer" aria-hidden="true" style={{ height }}>
      <td colSpan={columns} />
    </tr>
  )
}
