//! VAL-ROUND: lower declared result sites into the one shared rescale opcode.
use super::{AstMetadata, Node, Type, ZenTypeEnv, address};

pub(super) fn lower(root: &Node<'_>, expected: &Type, env: &ZenTypeEnv, metadata: &AstMetadata, source: &str) -> Result<String,String> {
    let mut edits = Vec::new();
    visit(root,expected,env,metadata,&mut edits)?;
    edits.sort_by_key(|(start,end,_,_)| (*start,*end));
    let mut output = String::new();
    let mut cursor = 0;
    for (start,end,scale,mode) in edits {
        let start = start as usize; let end = end as usize;
        if start < cursor || end > source.len() || start >= end { return Err("exact site has no faithful source span".into()); }
        output.push_str(source.get(cursor..start).ok_or("invalid site span")?);
        output.push_str("round(("); output.push_str(source.get(start..end).ok_or("invalid site span")?);
        output.push_str(&format!("), {scale}, '{mode}')"));
        cursor = end;
    }
    output.push_str(source.get(cursor..).ok_or("invalid site span")?);
    Ok(output)
}
fn visit(node: &Node<'_>, expected: &Type, env: &ZenTypeEnv, metadata: &AstMetadata, edits: &mut Vec<(u32,u32,u64,&'static str)>) -> Result<(),String> {
    if let Type::Optional(inner) = expected { return visit(node,inner,env,metadata,edits); }
    if matches!(node,Node::Null) { return Ok(()); }
    if let Type::Decimal {scale,rounding,..} = expected {
        let (start,end) = metadata.get(&address(node)).ok_or("missing exact site span")?.span;
        edits.push((start,end,*scale,rounding.canonical_field().unwrap_or("refuse"))); return Ok(());
    }
    match node {
        Node::Parenthesized(inner) => visit(inner,expected,env,metadata,edits),
        Node::Conditional {on_true,on_false,..} => { visit(on_true,expected,env,metadata,edits)?;visit(on_false,expected,env,metadata,edits) }
        Node::Object(fields) => {
            let Type::Record(name) = expected else { return Ok(()); };
            let record = env.record(name).ok_or("unknown destination record")?;
            for (key,value) in fields {
                let Node::String(key) = key else { return Err("exact destination needs literal fields".into()); };
                if let Some(field) = record.fields.get(*key) { visit(value,&field.ty,env,metadata,edits)?; }
            }
            Ok(())
        }
        Node::Array(items) => {
            if let Type::List(item) = expected { for value in items { visit(value,item,env,metadata,edits)?; } }
            Ok(())
        }
        _ => Ok(()),
    }
}
