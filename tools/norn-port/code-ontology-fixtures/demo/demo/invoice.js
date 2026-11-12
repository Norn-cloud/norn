// frappe.ui.form.on("Fake", { refresh(frm) { frm.doc.fake = 1; } });
const example = 'frappe.ui.form.on("Fake")';
frappe.ui.form.on("Invoice", {
  refresh(frm) { console.log(frm.doc.total); frm.set_value("status", "Paid"); },
  validate: function(frm) { frm.doc.status = "Checked"; }
});
frappe.ui.form.on(dynamicType, dynamicHandlers);
frappe.ui.form.on("Invoice", {
  changed: (frm) => { frm.doc.total += 1; },
  imported: externalHandlers,
  [dynamicEvent](frm) { frm.doc.total = 0; }
});
frappe.ui.form.on("Invoice", {
  scope_checks(frm) {
    frm.doc.total++;
    --frm.doc.total;
    function inner(frm) { frm.set_value("status", "X"); }
    const arrow = (frm) => { frm.set_value("status", "Y"); };
    frm.set_value("status", "outer");
  }
});

frappe.ui.form.on("Invoice", {
  catch_scope(frm) {
    try { external(); } catch (frm) {
      frm.set_value("status", "caught");
      console.log(frm.doc.total);
    }
    frm.set_value("status", "after catch");
  }
});
