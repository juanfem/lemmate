/** One heading in a note, as the editor found it. The margin index and the palette both
 *  order by `pos`, so it is the document offset rather than a line number. */
export interface OutlineItem {
  level: number
  text: string
  pos: number
}
