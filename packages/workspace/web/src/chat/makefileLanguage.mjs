import { StreamLanguage } from "@codemirror/language";

const DIRECTIVES = /^(?:define|else|endef|endif|export|ifdef|ifeq|ifndef|ifneq|include|-include|override|private|sinclude|undefine|unexport|vpath)\b/;
const TARGET = /^[^\s:#=]+(?=\s*(?::|::))/;
const VARIABLE_DEFINITION = /^[A-Za-z_][A-Za-z0-9_.-]*(?=\s*(?::=|::=|\?=|\+=|!=|=))/;
const VARIABLE_REFERENCE = /^\$(?:\([^)]*\)|\{[^}]*\}|[@%<?^+*|])/;

const parser = {
  name: "makefile",
  startState: () => ({ recipe: false }),
  token(stream, state) {
    if (stream.sol()) state.recipe = stream.peek() === "\t";
    if (stream.eatSpace()) return null;
    if (stream.match(/^#.*$/)) return "comment";
    if (stream.sol() && stream.match(DIRECTIVES)) return "keyword";
    if (stream.sol() && stream.match(TARGET)) return "labelName";
    if (stream.sol() && stream.match(VARIABLE_DEFINITION)) return "definition(variableName)";
    if (stream.match(VARIABLE_REFERENCE)) return "variableName";
    if (state.recipe && stream.match(/^[@+-]?(?:[A-Za-z_][A-Za-z0-9_.-]*)/)) return "operator";
    if (stream.match(/^"(?:[^"\\]|\\.)*"|^'(?:[^'\\]|\\.)*'/)) return "string";
    stream.next();
    return null;
  },
};

export function makefile() {
  return StreamLanguage.define(parser);
}
