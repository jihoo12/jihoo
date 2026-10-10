# Syntax summary

Every form of jihoo on one page, as an informal grammar. `x?` is optional,
`x*` repeats zero or more times, `x | y` is either, and `sep` is a comma or a
newline. The [Syntax](../language/syntax.md) page explains the lexical rules.

## Files

```text
file       = attribute* item*
attribute  = "#![" ("native" | "freestanding") "]"
item       = "import" path ("as" NAME)?
           | "pub"? "fn" NAME "(" params ")" ("->" type)? block
           | "pub"? "extern" "fn" NAME "(" params ("," "...")? ")" ("->" type)?
           | "pub"? "struct" NAME ("(" params ")")? "{" (NAME ":" type) sep* "}"
           | "pub"? "enum" NAME ("(" params ")")? "{" (NAME ("(" types ")")?) sep* "}"
           | "pub"? "const" NAME (":" type)? "=" expr
           | "pub"? "macro" NAME "(" params ")" "->" ("expr" | "stmts" | "items") block
           | path "!" "(" exprs ")"                       ; an item macro
params     = (("comptime")? NAME ":" type),*
path       = NAME ("." NAME)*
```

## Types

```text
type = "unit" | "bool" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64"
     | "f32" | "f64" | "str" | "type" | "expr" | "stmts" | "items"
     | path ("(" exprs ")")?                              ; struct or enum, maybe generic
     | "[" type ";" expr "]"                              ; array
     | "fn" "(" types ")" ("->" type)?                    ; function, `->` on the same line
     | "*" type | "ref" type | "cell" type | "chan" type
```

## Statements

A statement ends at a newline, `;` or `}`.

```text
block = "{" stmt* "}"
stmt  = "let" NAME (":" type)? "=" expr
      | place "=" expr
      | "if" expr block ("else" (block | if-stmt))?
      | "while" expr block
      | "break" | "continue"                              ; inside a `while`
      | "return" expr?
      | "match" expr "{" (pattern ("if" expr)? "=>" (block | stmt)) sep* "}"
      | "select" "{" select-arm sep* "}"
      | "go" call
      | path "!" "(" exprs ")"                            ; a statement macro
      | expr
select-arm = ("let" NAME "=")? "recv" "(" expr ")" "=>" (block | stmt)
           | "send" "(" expr "," expr ")" "=>" (block | stmt)
           | "_" "=>" (block | stmt)
place = NAME | place "." NAME | place "[" expr "]" | "*" expr
```

## Expressions

From the loosest-binding form to the tightest; see
[Operators](../language/operators.md#precedence) for the binary operators.

```text
expr    = expr binop expr
        | expr "as" type
        | ("-" | "!" | "*" | "&" | "ref" | "comptime") expr
        | expr "(" exprs ")"                              ; call
        | expr "[" expr "]" | expr "." NAME               ; index, field
        | primary
primary = INT | FLOAT | STRING | "true" | "false"
        | NAME | path                                     ; variable, constant, module item
        | path ("(" exprs ")")? "{" (NAME ":" expr) sep* "}"   ; struct literal
        | path "." NAME ("(" exprs ")")?                  ; enum variant: `Shape.Rect(1, 2)`
        | "[" exprs "]" | "[" expr ";" expr "]"           ; array literals
        | "(" expr ")"
        | "fn" "(" (NAME (":" type)?),* ")" ("->" type)? block   ; anonymous function
        | "match" expr "{" (pattern ("if" expr)? "=>" expr) sep* "}"
        | path "!" "(" exprs ")"                          ; an expression macro
        | "quote" ("(" expr ")" | block | "items" "{" item* "}")
        | "$" NAME | "$" "(" expr ")"                     ; a hole, inside quote
        | "size_of" "(" type ")" | "align_of" "(" type ")"
        | "chan" "(" type ("," expr)? ")" | "cell" "(" expr ")"
        | "asm" "(" STRING* ("," asm-operand)* ")"
        | type                                            ; a type argument: `max(u8, a, b)`
asm-operand = "out" "(" (STRING | "reg") ")" type
            | "in" "(" (STRING | "reg" | "out") ")" expr
            | "clobber" "(" STRING,* ")"
```

## Patterns

```text
pattern = single ("|" single)*
single  = "_" | NAME | INT | "-" INT | "true" | "false"
        | NAME "(" pattern,* ")"                          ; a variant with a payload
        | NAME "{" (NAME (":" pattern)?) sep* ".."? "}"   ; a struct, `..` for the rest
```

## Keywords

```text
as      asm     break   cell      chan    comptime  const   continue
else    enum    extern  false     fn      go        if      import
let     macro   match   pub       quote   ref       return  select
struct  true    while
```
