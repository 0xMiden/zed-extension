(constant
  name: (const_ident) @name) @item

(type_alias
  name: (identifier) @name) @item

(enum_declaration
  name: (identifier) @name) @item

(procedure
  name: (procedure_name
    (identifier) @name)) @item

(procedure
  name: (procedure_name
    (quoted_ident) @name)) @item

(entrypoint) @item

(annotation
  name: (identifier) @name) @annotation

(doc_comment) @annotation
