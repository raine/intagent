declare module "bash-parser" {
  interface LocationPoint {
    char: number
    row: number
    col: number
  }
  interface Location {
    start: LocationPoint
    end: LocationPoint
  }
  interface WordNode {
    type: "Word" | "AssignmentWord"
    text: string
    expansion?: unknown[]
    loc?: Location
  }
  interface CommandNode {
    type: "Command"
    name?: WordNode
    prefix?: unknown[]
    suffix?: Array<WordNode | { type: string }>
  }
  interface PipelineNode {
    type: "Pipeline"
    commands: unknown[]
  }
  interface ScriptNode {
    type: "Script"
    commands: unknown[]
  }
  export default function parse(
    source: string,
    options?: { insertLOC?: boolean },
  ): ScriptNode
}
