var __defProp = Object.defineProperty;
var __defNormalProp = (obj, key, value) => key in obj ? __defProp(obj, key, { enumerable: true, configurable: true, writable: true, value }) : obj[key] = value;
var __publicField = (obj, key, value) => __defNormalProp(obj, typeof key !== "symbol" ? key + "" : key, value);

// src/Renderer.tsx
import React, { createContext, useContext, useMemo, useRef, useState } from "react";

// src/updates.ts
function applyUiUpdates(component, updates) {
  return updates.reduce((current, update) => {
    if (!current) return null;
    return applyUiUpdate(current, update);
  }, component);
}
function applyUiUpdate(component, update) {
  if (component.id === update.target_id) {
    return applyUpdateToTarget(component, update);
  }
  switch (component.type) {
    case "stack": {
      const updated = applyToChildren(component.children, update);
      if (!updated.changed) return component;
      return { ...component, children: updated.children };
    }
    case "grid": {
      const updated = applyToChildren(component.children, update);
      if (!updated.changed) return component;
      return { ...component, children: updated.children };
    }
    case "container": {
      const updated = applyToChildren(component.children, update);
      if (!updated.changed) return component;
      return { ...component, children: updated.children };
    }
    case "card": {
      const contentUpdate = applyToChildren(component.content, update);
      const footerUpdate = component.footer ? applyToChildren(component.footer, update) : { children: component.footer, changed: false };
      if (!contentUpdate.changed && !footerUpdate.changed) return component;
      return {
        ...component,
        content: contentUpdate.children,
        footer: footerUpdate.children
      };
    }
    case "tabs": {
      let changed = false;
      const tabs = component.tabs.map((tab) => {
        const updated = applyToChildren(tab.content, update);
        if (updated.changed) {
          changed = true;
          return { ...tab, content: updated.children };
        }
        return tab;
      });
      if (!changed) return component;
      return { ...component, tabs };
    }
    case "modal": {
      const contentUpdate = applyToChildren(component.content, update);
      const footerUpdate = component.footer ? applyToChildren(component.footer, update) : { children: component.footer, changed: false };
      if (!contentUpdate.changed && !footerUpdate.changed) return component;
      return {
        ...component,
        content: contentUpdate.children,
        footer: footerUpdate.children
      };
    }
    default:
      return component;
  }
}
function applyUpdateToTarget(component, update) {
  switch (update.operation) {
    case "remove":
      return null;
    case "replace":
      return update.payload ?? component;
    case "patch":
      if (!update.payload) return component;
      return {
        ...component,
        ...update.payload,
        id: update.payload.id ?? component.id
      };
    case "append":
      if (!update.payload) return component;
      return appendChild(component, update.payload);
    default:
      return component;
  }
}
function appendChild(component, child) {
  switch (component.type) {
    case "stack":
      return { ...component, children: [...component.children, child] };
    case "grid":
      return { ...component, children: [...component.children, child] };
    case "container":
      return { ...component, children: [...component.children, child] };
    case "card":
      return { ...component, content: [...component.content, child] };
    case "tabs":
      return component;
    case "modal":
      return { ...component, content: [...component.content, child] };
    default:
      return component;
  }
}
function applyToChildren(children, update) {
  let changed = false;
  const next = children.flatMap((child) => {
    const updated = applyUiUpdate(child, update);
    if (!updated) {
      changed = true;
      return [];
    }
    if (updated !== child) {
      changed = true;
    }
    return [updated];
  });
  return { children: next, changed };
}

// src/Renderer.tsx
import { AlertCircle, CheckCircle, Info, XCircle, User, Mail, Calendar } from "lucide-react";
import Markdown from "react-markdown";
import clsx from "clsx";
import {
  BarChart,
  LineChart,
  AreaChart,
  PieChart,
  Bar,
  Line,
  Area,
  Pie,
  Cell,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer
} from "recharts";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
var IconMap = {
  "alert-circle": AlertCircle,
  "check-circle": CheckCircle,
  "info": Info,
  "x-circle": XCircle,
  "user": User,
  "mail": Mail,
  "calendar": Calendar
};
var FormContext = createContext({});
var Renderer = ({ component, onAction, theme }) => {
  const isDark = theme === "dark";
  return /* @__PURE__ */ jsx(FormContext.Provider, { value: { onAction }, children: /* @__PURE__ */ jsx("div", { className: isDark ? "dark" : "", children: /* @__PURE__ */ jsx(ComponentRenderer, { component }) }) });
};
var StreamingRenderer = ({ component, updates, onAction, theme }) => {
  const [current, setCurrent] = useState(component);
  const updatesList = useMemo(() => {
    if (!updates) return [];
    return Array.isArray(updates) ? updates : [updates];
  }, [updates]);
  React.useEffect(() => {
    setCurrent(component);
  }, [component]);
  React.useEffect(() => {
    if (updatesList.length === 0) return;
    setCurrent((prev) => {
      const updated = applyUiUpdates(prev, updatesList);
      return updated ?? prev;
    });
  }, [updatesList]);
  return /* @__PURE__ */ jsx(Renderer, { component: current, onAction, theme });
};
var ComponentRenderer = ({ component }) => {
  const { onAction } = useContext(FormContext);
  const formRef = useRef(null);
  const handleButtonClick = (actionId) => {
    if (formRef.current) {
      const formData = new FormData(formRef.current);
      const data = {};
      formData.forEach((value, key) => {
        data[key] = value;
      });
      onAction?.({ action: "form_submit", action_id: actionId, data });
    } else {
      onAction?.({ action: "button_click", action_id: actionId });
    }
  };
  switch (component.type) {
    case "text":
      if (component.variant === "body" || !component.variant) {
        return /* @__PURE__ */ jsx("div", { className: "prose prose-sm dark:prose-invert max-w-none text-gray-700 dark:text-gray-300", children: /* @__PURE__ */ jsx(Markdown, { children: component.content }) });
      }
      const Tag = component.variant === "h1" ? "h1" : component.variant === "h2" ? "h2" : component.variant === "h3" ? "h3" : component.variant === "h4" ? "h4" : component.variant === "code" ? "code" : "p";
      const classes = clsx({
        "text-4xl font-bold mb-4 dark:text-white": component.variant === "h1",
        "text-3xl font-bold mb-3 dark:text-white": component.variant === "h2",
        "text-2xl font-bold mb-2 dark:text-white": component.variant === "h3",
        "text-xl font-bold mb-2 dark:text-white": component.variant === "h4",
        "font-mono bg-gray-100 dark:bg-gray-800 p-1 rounded dark:text-gray-100": component.variant === "code",
        "text-sm text-gray-500 dark:text-gray-400": component.variant === "caption"
      });
      return /* @__PURE__ */ jsx(Tag, { className: classes, children: component.content });
    case "button":
      const btnClasses = clsx("px-4 py-2 rounded font-medium transition-colors", {
        "bg-blue-600 text-white hover:bg-blue-700": component.variant === "primary" || !component.variant,
        "bg-gray-200 text-gray-800 hover:bg-gray-300": component.variant === "secondary",
        "bg-red-600 text-white hover:bg-red-700": component.variant === "danger",
        "bg-transparent hover:bg-gray-100": component.variant === "ghost",
        "border border-gray-300 hover:bg-gray-50": component.variant === "outline",
        "opacity-50 cursor-not-allowed": component.disabled
      });
      return /* @__PURE__ */ jsx(
        "button",
        {
          type: "button",
          className: btnClasses,
          disabled: component.disabled,
          onClick: () => handleButtonClick(component.action_id),
          children: component.label
        }
      );
    case "icon":
      const Icon = IconMap[component.name] || Info;
      return /* @__PURE__ */ jsx(Icon, { size: component.size || 24 });
    case "alert":
      const alertClasses = clsx("p-4 rounded-md border mb-4 flex items-start gap-3", {
        "bg-blue-50 border-blue-200 text-blue-800": component.variant === "info" || !component.variant,
        "bg-green-50 border-green-200 text-green-800": component.variant === "success",
        "bg-yellow-50 border-yellow-200 text-yellow-800": component.variant === "warning",
        "bg-red-50 border-red-200 text-red-800": component.variant === "error"
      });
      const AlertIcon = component.variant === "success" ? CheckCircle : component.variant === "warning" ? AlertCircle : component.variant === "error" ? XCircle : Info;
      return /* @__PURE__ */ jsxs("div", { className: alertClasses, children: [
        /* @__PURE__ */ jsx(AlertIcon, { className: "w-5 h-5 mt-0.5" }),
        /* @__PURE__ */ jsxs("div", { children: [
          /* @__PURE__ */ jsx("div", { className: "font-semibold", children: component.title }),
          component.description && /* @__PURE__ */ jsx("div", { className: "text-sm mt-1 opacity-90", children: component.description })
        ] })
      ] });
    case "card":
      const hasInputs = component.content.some(
        (c) => c.type === "text_input" || c.type === "number_input" || c.type === "select" || c.type === "switch" || c.type === "textarea"
      );
      const handleSubmit = (e) => {
        e.preventDefault();
        const formData = new FormData(e.currentTarget);
        const data = {};
        formData.forEach((value, key) => {
          data[key] = value;
        });
        const submitBtn = [...component.content, ...component.footer || []].find(
          (c) => c.type === "button"
        );
        onAction?.({
          action: "form_submit",
          action_id: submitBtn?.action_id || "form_submit",
          data
        });
      };
      const cardContent = /* @__PURE__ */ jsxs(Fragment, { children: [
        (component.title || component.description) && /* @__PURE__ */ jsxs("div", { className: "p-4 border-b dark:border-gray-700 bg-gray-50 dark:bg-gray-800", children: [
          component.title && /* @__PURE__ */ jsx("h3", { className: "font-semibold text-lg dark:text-white", children: component.title }),
          component.description && /* @__PURE__ */ jsx("p", { className: "text-gray-500 dark:text-gray-400 text-sm", children: component.description })
        ] }),
        /* @__PURE__ */ jsx("div", { className: "p-4", children: component.content.map((child, i) => /* @__PURE__ */ jsx(ComponentRenderer, { component: child }, i)) }),
        component.footer && /* @__PURE__ */ jsx("div", { className: "p-4 border-t dark:border-gray-700 bg-gray-50 dark:bg-gray-800 flex gap-2 justify-end", children: component.footer.map((child, i) => /* @__PURE__ */ jsx(ComponentRenderer, { component: child }, i)) })
      ] });
      return hasInputs ? /* @__PURE__ */ jsx("form", { onSubmit: handleSubmit, className: "bg-white dark:bg-gray-900 rounded-lg border dark:border-gray-700 shadow-sm overflow-hidden mb-4", children: cardContent }) : /* @__PURE__ */ jsx("div", { className: "bg-white dark:bg-gray-900 rounded-lg border dark:border-gray-700 shadow-sm overflow-hidden mb-4", children: cardContent });
    case "stack":
      const stackClasses = clsx("flex", {
        "flex-col": component.direction === "vertical",
        "flex-row": component.direction === "horizontal"
      });
      return /* @__PURE__ */ jsx("div", { className: stackClasses, style: { gap: (component.gap || 4) * 4 }, children: component.children.map((child, i) => /* @__PURE__ */ jsx(ComponentRenderer, { component: child }, i)) });
    case "text_input":
      const inputType = component.input_type || "text";
      return /* @__PURE__ */ jsxs("div", { className: "mb-3", children: [
        /* @__PURE__ */ jsx("label", { className: "block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1", children: component.label }),
        /* @__PURE__ */ jsx(
          "input",
          {
            type: inputType,
            name: component.name,
            placeholder: component.placeholder,
            defaultValue: component.default_value,
            required: component.required,
            onChange: (event) => onAction?.({
              action: "input_change",
              name: component.name,
              value: event.currentTarget.value
            }),
            className: clsx("w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none bg-white dark:bg-gray-800 dark:border-gray-600 dark:text-white", {
              "border-red-500 focus:ring-red-500 focus:border-red-500": component.error
            })
          }
        ),
        component.error && /* @__PURE__ */ jsx("p", { className: "text-red-500 dark:text-red-400 text-sm mt-1", children: component.error })
      ] });
    case "number_input":
      return /* @__PURE__ */ jsxs("div", { className: "mb-3", children: [
        /* @__PURE__ */ jsx("label", { className: "block text-sm font-medium text-gray-700 mb-1", children: component.label }),
        /* @__PURE__ */ jsx(
          "input",
          {
            type: "number",
            name: component.name,
            min: component.min,
            max: component.max,
            step: component.step,
            required: component.required,
            onChange: (event) => {
              const parsed = event.currentTarget.valueAsNumber;
              onAction?.({
                action: "input_change",
                name: component.name,
                value: Number.isNaN(parsed) ? event.currentTarget.value : parsed
              });
            },
            className: clsx("w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none", {
              "border-red-500 focus:ring-red-500 focus:border-red-500": component.error
            })
          }
        ),
        component.error && /* @__PURE__ */ jsx("p", { className: "text-red-500 text-sm mt-1", children: component.error })
      ] });
    case "select":
      return /* @__PURE__ */ jsxs("div", { className: "mb-3", children: [
        /* @__PURE__ */ jsx("label", { className: "block text-sm font-medium text-gray-700 mb-1", children: component.label }),
        /* @__PURE__ */ jsxs(
          "select",
          {
            name: component.name,
            required: component.required,
            onChange: (event) => onAction?.({
              action: "input_change",
              name: component.name,
              value: event.currentTarget.value
            }),
            className: clsx("w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none", {
              "border-red-500 focus:ring-red-500 focus:border-red-500": component.error
            }),
            children: [
              /* @__PURE__ */ jsx("option", { value: "", children: "Select..." }),
              component.options.map((opt, i) => /* @__PURE__ */ jsx("option", { value: opt.value, children: opt.label }, i))
            ]
          }
        ),
        component.error && /* @__PURE__ */ jsx("p", { className: "text-red-500 text-sm mt-1", children: component.error })
      ] });
    case "switch":
      return /* @__PURE__ */ jsxs("div", { className: "mb-3 flex items-center", children: [
        /* @__PURE__ */ jsx(
          "input",
          {
            type: "checkbox",
            name: component.name,
            defaultChecked: component.default_checked,
            onChange: (event) => onAction?.({
              action: "input_change",
              name: component.name,
              value: event.currentTarget.checked
            }),
            className: "h-4 w-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
          }
        ),
        /* @__PURE__ */ jsx("label", { className: "ml-2 text-sm font-medium text-gray-700", children: component.label })
      ] });
    case "multi_select":
      return /* @__PURE__ */ jsxs("div", { className: "mb-3", children: [
        /* @__PURE__ */ jsx("label", { className: "block text-sm font-medium text-gray-700 mb-1", children: component.label }),
        /* @__PURE__ */ jsx(
          "select",
          {
            name: component.name,
            multiple: true,
            required: component.required,
            size: Math.min(component.options.length, 5),
            onChange: (event) => {
              const selected = Array.from(event.currentTarget.selectedOptions).map((opt) => opt.value);
              onAction?.({
                action: "input_change",
                name: component.name,
                value: selected
              });
            },
            className: "w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none",
            children: component.options.map((opt, i) => /* @__PURE__ */ jsx("option", { value: opt.value, children: opt.label }, i))
          }
        )
      ] });
    case "date_input":
      return /* @__PURE__ */ jsxs("div", { className: "mb-3", children: [
        /* @__PURE__ */ jsx("label", { className: "block text-sm font-medium text-gray-700 mb-1", children: component.label }),
        /* @__PURE__ */ jsx(
          "input",
          {
            type: "date",
            name: component.name,
            required: component.required,
            onChange: (event) => onAction?.({
              action: "input_change",
              name: component.name,
              value: event.currentTarget.value
            }),
            className: "w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none"
          }
        )
      ] });
    case "slider":
      return /* @__PURE__ */ jsxs("div", { className: "mb-3", children: [
        /* @__PURE__ */ jsx("label", { className: "block text-sm font-medium text-gray-700 mb-1", children: component.label }),
        /* @__PURE__ */ jsx(
          "input",
          {
            type: "range",
            name: component.name,
            min: component.min,
            max: component.max,
            step: component.step,
            defaultValue: component.default_value,
            onChange: (event) => {
              const parsed = event.currentTarget.valueAsNumber;
              onAction?.({
                action: "input_change",
                name: component.name,
                value: Number.isNaN(parsed) ? event.currentTarget.value : parsed
              });
            },
            className: "w-full h-2 bg-gray-200 rounded-lg appearance-none cursor-pointer"
          }
        )
      ] });
    case "progress":
      return /* @__PURE__ */ jsxs("div", { className: "mb-3", children: [
        component.label && /* @__PURE__ */ jsx("div", { className: "text-sm text-gray-600 mb-1", children: component.label }),
        /* @__PURE__ */ jsx("div", { className: "w-full bg-gray-200 rounded-full h-2.5", children: /* @__PURE__ */ jsx(
          "div",
          {
            className: "bg-blue-600 h-2.5 rounded-full transition-all",
            style: { width: `${component.value}%` }
          }
        ) })
      ] });
    case "textarea":
      return /* @__PURE__ */ jsxs("div", { className: "mb-3", children: [
        /* @__PURE__ */ jsx("label", { className: "block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1", children: component.label }),
        /* @__PURE__ */ jsx(
          "textarea",
          {
            name: component.name,
            placeholder: component.placeholder,
            rows: component.rows || 4,
            required: component.required,
            defaultValue: component.default_value,
            onChange: (event) => onAction?.({
              action: "input_change",
              name: component.name,
              value: event.currentTarget.value
            }),
            className: clsx("w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none bg-white dark:bg-gray-800 dark:border-gray-600 dark:text-white resize-y", {
              "border-red-500 focus:ring-red-500 focus:border-red-500": component.error
            })
          }
        ),
        component.error && /* @__PURE__ */ jsx("p", { className: "text-red-500 dark:text-red-400 text-sm mt-1", children: component.error })
      ] });
    case "spinner":
      const spinnerSizes = {
        small: "w-4 h-4",
        medium: "w-8 h-8",
        large: "w-12 h-12"
      };
      return /* @__PURE__ */ jsxs("div", { className: "flex items-center gap-2", children: [
        /* @__PURE__ */ jsx("div", { className: clsx("animate-spin rounded-full border-2 border-blue-600 border-t-transparent", spinnerSizes[component.size || "medium"]) }),
        component.label && /* @__PURE__ */ jsx("span", { className: "text-gray-600 dark:text-gray-400", children: component.label })
      ] });
    case "skeleton":
      return /* @__PURE__ */ jsx(
        "div",
        {
          className: clsx("animate-pulse bg-gray-200 dark:bg-gray-700", {
            "h-4 rounded": component.variant === "text" || !component.variant,
            "rounded-full aspect-square": component.variant === "circle",
            "rounded": component.variant === "rectangle"
          }),
          style: { width: component.width || "100%", height: component.height }
        }
      );
    case "toast":
      const toastClasses = clsx("fixed bottom-4 right-4 p-4 rounded-lg shadow-lg flex items-center gap-3 z-50", {
        "bg-blue-50 border border-blue-200 text-blue-800": component.variant === "info" || !component.variant,
        "bg-green-50 border border-green-200 text-green-800": component.variant === "success",
        "bg-yellow-50 border border-yellow-200 text-yellow-800": component.variant === "warning",
        "bg-red-50 border border-red-200 text-red-800": component.variant === "error"
      });
      const ToastIcon = component.variant === "success" ? CheckCircle : component.variant === "warning" ? AlertCircle : component.variant === "error" ? XCircle : Info;
      return /* @__PURE__ */ jsxs("div", { className: toastClasses, children: [
        /* @__PURE__ */ jsx(ToastIcon, { className: "w-5 h-5" }),
        /* @__PURE__ */ jsx("span", { children: component.message }),
        component.dismissible !== false && /* @__PURE__ */ jsx(
          "button",
          {
            onClick: () => onAction?.({ action: "button_click", action_id: "toast_dismiss" }),
            className: "ml-2 text-gray-500 hover:text-gray-700",
            children: /* @__PURE__ */ jsx(XCircle, { className: "w-4 h-4" })
          }
        )
      ] });
    case "modal":
      const modalSizes = {
        small: "max-w-sm",
        medium: "max-w-lg",
        large: "max-w-2xl",
        full: "max-w-full mx-4"
      };
      return /* @__PURE__ */ jsx("div", { className: "fixed inset-0 bg-black/50 flex items-center justify-center z-50", children: /* @__PURE__ */ jsxs("div", { className: clsx("bg-white dark:bg-gray-900 rounded-lg shadow-xl w-full", modalSizes[component.size || "medium"]), children: [
        /* @__PURE__ */ jsxs("div", { className: "p-4 border-b dark:border-gray-700 flex justify-between items-center", children: [
          /* @__PURE__ */ jsx("h3", { className: "font-semibold text-lg dark:text-white", children: component.title }),
          component.closable !== false && /* @__PURE__ */ jsx(
            "button",
            {
              onClick: () => onAction?.({ action: "button_click", action_id: "modal_close" }),
              className: "text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200",
              children: /* @__PURE__ */ jsx(XCircle, { className: "w-5 h-5" })
            }
          )
        ] }),
        /* @__PURE__ */ jsx("div", { className: "p-4", children: component.content.map((child, i) => /* @__PURE__ */ jsx(ComponentRenderer, { component: child }, i)) }),
        component.footer && /* @__PURE__ */ jsx("div", { className: "p-4 border-t dark:border-gray-700 flex justify-end gap-2", children: component.footer.map((child, i) => /* @__PURE__ */ jsx(ComponentRenderer, { component: child }, i)) })
      ] }) });
    case "grid":
      return /* @__PURE__ */ jsx(
        "div",
        {
          className: "grid gap-4 mb-4",
          style: { gridTemplateColumns: `repeat(${component.columns || 2}, 1fr)` },
          children: component.children.map((child, i) => /* @__PURE__ */ jsx(ComponentRenderer, { component: child }, i))
        }
      );
    case "list":
      return /* @__PURE__ */ jsx("ul", { className: "space-y-2 mb-4 list-disc list-inside", children: component.items.map((item, i) => /* @__PURE__ */ jsx("li", { className: "text-gray-700", children: item }, i)) });
    case "key_value":
      return /* @__PURE__ */ jsx("dl", { className: "grid grid-cols-2 gap-x-4 gap-y-2 mb-4", children: component.pairs.map((pair, i) => /* @__PURE__ */ jsxs(React.Fragment, { children: [
        /* @__PURE__ */ jsxs("dt", { className: "font-medium text-gray-700", children: [
          pair.key,
          ":"
        ] }),
        /* @__PURE__ */ jsx("dd", { className: "text-gray-900", children: pair.value })
      ] }, i)) });
    case "tabs":
      const [activeTab, setActiveTab] = React.useState(0);
      return /* @__PURE__ */ jsxs("div", { className: "mb-4", children: [
        /* @__PURE__ */ jsx("div", { className: "border-b border-gray-200", children: /* @__PURE__ */ jsx("nav", { className: "flex space-x-4", children: component.tabs.map((tab, i) => /* @__PURE__ */ jsx(
          "button",
          {
            onClick: () => {
              setActiveTab(i);
              onAction?.({ action: "tab_change", index: i });
            },
            className: clsx("px-4 py-2 border-b-2 font-medium text-sm transition-colors", {
              "border-blue-600 text-blue-600": activeTab === i,
              "border-transparent text-gray-500 hover:text-gray-700": activeTab !== i
            }),
            children: tab.label
          },
          i
        )) }) }),
        /* @__PURE__ */ jsx("div", { className: "p-4", children: component.tabs[activeTab].content.map(
          (child, i) => /* @__PURE__ */ jsx(ComponentRenderer, { component: child }, i)
        ) })
      ] });
    case "table":
      const [sortColumn, setSortColumn] = React.useState(null);
      const [sortDirection, setSortDirection] = React.useState("asc");
      const [currentPage, setCurrentPage] = React.useState(0);
      const handleSort = (accessorKey) => {
        if (!component.sortable) return;
        if (sortColumn === accessorKey) {
          setSortDirection(sortDirection === "asc" ? "desc" : "asc");
        } else {
          setSortColumn(accessorKey);
          setSortDirection("asc");
        }
      };
      let tableData = [...component.data];
      if (sortColumn) {
        tableData.sort((a, b) => {
          const aVal = a[sortColumn] ?? "";
          const bVal = b[sortColumn] ?? "";
          const cmp = String(aVal).localeCompare(String(bVal));
          return sortDirection === "asc" ? cmp : -cmp;
        });
      }
      const pageSize = component.page_size || tableData.length;
      const totalPages = Math.ceil(tableData.length / pageSize);
      const paginatedData = tableData.slice(currentPage * pageSize, (currentPage + 1) * pageSize);
      return /* @__PURE__ */ jsxs("div", { className: "mb-4 overflow-x-auto", children: [
        /* @__PURE__ */ jsxs("table", { className: clsx("min-w-full divide-y divide-gray-200 dark:divide-gray-700 border dark:border-gray-700 rounded-lg overflow-hidden"), children: [
          /* @__PURE__ */ jsx("thead", { className: "bg-gray-50 dark:bg-gray-800", children: /* @__PURE__ */ jsx("tr", { children: component.columns.map((col, i) => /* @__PURE__ */ jsxs(
            "th",
            {
              onClick: () => handleSort(col.accessor_key),
              className: clsx(
                "px-4 py-3 text-left text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider",
                component.sortable && col.sortable !== false && "cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-700"
              ),
              children: [
                col.header,
                sortColumn === col.accessor_key && /* @__PURE__ */ jsx("span", { className: "ml-1", children: sortDirection === "asc" ? "\u2191" : "\u2193" })
              ]
            },
            i
          )) }) }),
          /* @__PURE__ */ jsx("tbody", { className: "bg-white dark:bg-gray-900 divide-y divide-gray-200 dark:divide-gray-700", children: paginatedData.map((row, ri) => /* @__PURE__ */ jsx("tr", { className: clsx(
            "hover:bg-gray-50 dark:hover:bg-gray-800",
            component.striped && ri % 2 === 1 && "bg-gray-50 dark:bg-gray-800/50"
          ), children: component.columns.map((col, ci) => /* @__PURE__ */ jsx("td", { className: "px-4 py-3 text-sm text-gray-700 dark:text-gray-300", children: String(row[col.accessor_key] ?? "") }, ci)) }, ri)) })
        ] }),
        component.page_size && totalPages > 1 && /* @__PURE__ */ jsxs("div", { className: "flex items-center justify-between mt-2 px-2", children: [
          /* @__PURE__ */ jsxs("span", { className: "text-sm text-gray-500 dark:text-gray-400", children: [
            "Page ",
            currentPage + 1,
            " of ",
            totalPages
          ] }),
          /* @__PURE__ */ jsxs("div", { className: "flex gap-2", children: [
            /* @__PURE__ */ jsx(
              "button",
              {
                onClick: () => setCurrentPage(Math.max(0, currentPage - 1)),
                disabled: currentPage === 0,
                className: "px-3 py-1 text-sm border rounded hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-50 dark:border-gray-600 dark:text-gray-300",
                children: "Previous"
              }
            ),
            /* @__PURE__ */ jsx(
              "button",
              {
                onClick: () => setCurrentPage(Math.min(totalPages - 1, currentPage + 1)),
                disabled: currentPage === totalPages - 1,
                className: "px-3 py-1 text-sm border rounded hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-50 dark:border-gray-600 dark:text-gray-300",
                children: "Next"
              }
            )
          ] })
        ] })
      ] });
    case "chart":
      const DEFAULT_COLORS = ["#3B82F6", "#10B981", "#F59E0B", "#EF4444", "#8B5CF6", "#EC4899", "#06B6D4"];
      const chartColors = component.colors || DEFAULT_COLORS;
      const chartKind = component.kind || "bar";
      const showLegend = component.show_legend !== false;
      return /* @__PURE__ */ jsxs("div", { className: "mb-4 p-4 bg-white dark:bg-gray-900 border dark:border-gray-700 rounded-lg", children: [
        component.title && /* @__PURE__ */ jsx("h4", { className: "font-semibold text-lg mb-4 dark:text-white", children: component.title }),
        /* @__PURE__ */ jsx(ResponsiveContainer, { width: "100%", height: 300, children: chartKind === "line" ? /* @__PURE__ */ jsxs(LineChart, { data: component.data, children: [
          /* @__PURE__ */ jsx(CartesianGrid, { strokeDasharray: "3 3" }),
          /* @__PURE__ */ jsx(XAxis, { dataKey: component.x_key, label: component.x_label ? { value: component.x_label, position: "bottom" } : void 0 }),
          /* @__PURE__ */ jsx(YAxis, { label: component.y_label ? { value: component.y_label, angle: -90, position: "insideLeft" } : void 0 }),
          /* @__PURE__ */ jsx(Tooltip, {}),
          showLegend && /* @__PURE__ */ jsx(Legend, {}),
          component.y_keys.map((key, i) => /* @__PURE__ */ jsx(Line, { type: "monotone", dataKey: key, stroke: chartColors[i % chartColors.length], strokeWidth: 2 }, key))
        ] }) : chartKind === "area" ? /* @__PURE__ */ jsxs(AreaChart, { data: component.data, children: [
          /* @__PURE__ */ jsx(CartesianGrid, { strokeDasharray: "3 3" }),
          /* @__PURE__ */ jsx(XAxis, { dataKey: component.x_key, label: component.x_label ? { value: component.x_label, position: "bottom" } : void 0 }),
          /* @__PURE__ */ jsx(YAxis, { label: component.y_label ? { value: component.y_label, angle: -90, position: "insideLeft" } : void 0 }),
          /* @__PURE__ */ jsx(Tooltip, {}),
          showLegend && /* @__PURE__ */ jsx(Legend, {}),
          component.y_keys.map((key, i) => /* @__PURE__ */ jsx(Area, { type: "monotone", dataKey: key, fill: chartColors[i % chartColors.length], fillOpacity: 0.6, stroke: chartColors[i % chartColors.length] }, key))
        ] }) : chartKind === "pie" ? /* @__PURE__ */ jsxs(PieChart, { children: [
          /* @__PURE__ */ jsx(
            Pie,
            {
              data: component.data,
              dataKey: component.y_keys[0],
              nameKey: component.x_key,
              cx: "50%",
              cy: "50%",
              outerRadius: 100,
              label: ({ name, percent }) => `${name}: ${((percent ?? 0) * 100).toFixed(0)}%`,
              children: component.data.map((_, i) => /* @__PURE__ */ jsx(Cell, { fill: chartColors[i % chartColors.length] }, i))
            }
          ),
          /* @__PURE__ */ jsx(Tooltip, {}),
          showLegend && /* @__PURE__ */ jsx(Legend, {})
        ] }) : /* @__PURE__ */ jsxs(BarChart, { data: component.data, children: [
          /* @__PURE__ */ jsx(CartesianGrid, { strokeDasharray: "3 3" }),
          /* @__PURE__ */ jsx(XAxis, { dataKey: component.x_key, label: component.x_label ? { value: component.x_label, position: "bottom" } : void 0 }),
          /* @__PURE__ */ jsx(YAxis, { label: component.y_label ? { value: component.y_label, angle: -90, position: "insideLeft" } : void 0 }),
          /* @__PURE__ */ jsx(Tooltip, {}),
          showLegend && /* @__PURE__ */ jsx(Legend, {}),
          component.y_keys.map((key, i) => /* @__PURE__ */ jsx(Bar, { dataKey: key, fill: chartColors[i % chartColors.length] }, key))
        ] }) })
      ] });
    case "code_block":
      return /* @__PURE__ */ jsx("div", { className: "mb-4", children: /* @__PURE__ */ jsx("pre", { className: "bg-gray-900 text-gray-100 p-4 rounded-lg overflow-x-auto text-sm", children: /* @__PURE__ */ jsx("code", { children: component.code }) }) });
    case "image":
      return /* @__PURE__ */ jsx("div", { className: "mb-4", children: /* @__PURE__ */ jsx(
        "img",
        {
          src: component.src,
          alt: component.alt || "",
          className: "max-w-full h-auto rounded-lg"
        }
      ) });
    case "badge":
      const badgeClasses = clsx("inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium", {
        "bg-gray-100 text-gray-800": component.variant === "default" || !component.variant,
        "bg-blue-100 text-blue-800": component.variant === "info",
        "bg-green-100 text-green-800": component.variant === "success",
        "bg-yellow-100 text-yellow-800": component.variant === "warning",
        "bg-red-100 text-red-800": component.variant === "error",
        "bg-gray-200 text-gray-700": component.variant === "secondary",
        "bg-transparent border border-gray-300 text-gray-700": component.variant === "outline"
      });
      return /* @__PURE__ */ jsx("span", { className: badgeClasses, children: component.label });
    case "divider":
      return /* @__PURE__ */ jsx("hr", { className: "my-4 border-gray-200" });
    case "container":
      return /* @__PURE__ */ jsx("div", { className: "max-w-7xl mx-auto px-4 sm:px-6 lg:px-8", children: component.children.map((child, i) => /* @__PURE__ */ jsx(ComponentRenderer, { component: child }, i)) });
    default:
      return /* @__PURE__ */ jsxs("div", { className: "text-red-500 text-sm p-2 border border-red-200 rounded", children: [
        "Unknown component: ",
        component.type
      ] });
  }
};

// src/a2ui/renderer.tsx
import React2, { createContext as createContext2, useContext as useContext2, useState as useState2 } from "react";
import clsx2 from "clsx";
import Markdown2 from "react-markdown";
import { AlertCircle as AlertCircle2, CheckCircle as CheckCircle2, Info as Info2, XCircle as XCircle2, User as User2, Mail as Mail2, Calendar as Calendar2 } from "lucide-react";

// src/a2ui/bindings.ts
var DEFAULT_FUNCTIONS = {
  now: () => (/* @__PURE__ */ new Date()).toISOString(),
  concat: (args) => args.map((value) => stringifyValue(value)).join(""),
  add: (args) => args.reduce((total, value) => total + toNumber(value), 0),
  formatString: (args, ctx) => formatString(String(args[0] ?? ""), ctx)
};
function isDataBinding(value) {
  return typeof value === "object" && value !== null && "path" in value && typeof value.path === "string" && Object.keys(value).length === 1;
}
function isFunctionCall(value) {
  return typeof value === "object" && value !== null && "call" in value && typeof value.call === "string";
}
function resolvePath(dataModel, path, scope) {
  const source = path.startsWith("/") ? dataModel : scope ?? dataModel;
  if (path === "/" || path.length === 0) {
    return source;
  }
  const tokens = path.replace(/^\//, "").split("/").filter(Boolean);
  let cursor = source;
  for (const token of tokens) {
    if (typeof cursor !== "object" || cursor === null) {
      return void 0;
    }
    cursor = cursor[token];
  }
  return cursor;
}
function resolveDynamicValue(value, dataModel, scope, functions) {
  if (isDataBinding(value)) {
    return resolvePath(dataModel, value.path, scope);
  }
  if (isFunctionCall(value)) {
    return evaluateFunctionCall(value, { dataModel, scope, functions });
  }
  return value;
}
function resolveDynamicString(value, dataModel, scope, functions) {
  const resolved = resolveDynamicValue(value, dataModel, scope, functions);
  return stringifyValue(resolved);
}
function evaluateFunctionCall(call, ctx) {
  const registry = { ...DEFAULT_FUNCTIONS, ...ctx.functions ?? {} };
  const fn = registry[call.call];
  if (!fn) {
    return void 0;
  }
  const args = (call.args ?? []).map(
    (arg) => resolveDynamicValue(arg, ctx.dataModel, ctx.scope, ctx.functions)
  );
  return fn(args, ctx);
}
function formatString(template, ctx) {
  let output = "";
  let index = 0;
  while (index < template.length) {
    if (template[index] === "\\" && template[index + 1] === "$" && template[index + 2] === "{") {
      output += "${";
      index += 3;
      continue;
    }
    if (template[index] === "$" && template[index + 1] === "{") {
      const { expression, nextIndex } = parseExpression(template, index + 2);
      const value = resolveExpression(expression, ctx);
      output += stringifyValue(value);
      index = nextIndex + 1;
      continue;
    }
    output += template[index];
    index += 1;
  }
  return output;
}
function parseExpression(source, startIndex) {
  let index = startIndex;
  let depth = 1;
  let inString = null;
  while (index < source.length) {
    const char = source[index];
    if (inString) {
      if (char === "\\") {
        index += 2;
        continue;
      }
      if (char === inString) {
        inString = null;
      }
      index += 1;
      continue;
    }
    if (char === '"' || char === "'") {
      inString = char;
      index += 1;
      continue;
    }
    if (char === "$" && source[index + 1] === "{") {
      depth += 1;
      index += 2;
      continue;
    }
    if (char === "}") {
      depth -= 1;
      if (depth === 0) {
        return { expression: source.slice(startIndex, index), nextIndex: index };
      }
    }
    index += 1;
  }
  return { expression: source.slice(startIndex), nextIndex: source.length - 1 };
}
function resolveExpression(expression, ctx) {
  const trimmed = expression.trim();
  if (trimmed.startsWith("/")) {
    return resolvePath(ctx.dataModel, trimmed, ctx.scope);
  }
  if (trimmed.length === 0) {
    return "";
  }
  const callMatch = /^([a-zA-Z_][\w]*)\((.*)\)$/.exec(trimmed);
  if (callMatch) {
    const [, name, rawArgs] = callMatch;
    const args = splitArgs(rawArgs).map((arg) => resolveArgument(arg, ctx));
    return evaluateFunctionCall({ call: name, args }, ctx);
  }
  return resolvePath(ctx.dataModel, trimmed, ctx.scope);
}
function splitArgs(raw) {
  const args = [];
  let current = "";
  let depth = 0;
  let inString = null;
  for (let index = 0; index < raw.length; index += 1) {
    const char = raw[index];
    if (inString) {
      current += char;
      if (char === "\\") {
        current += raw[index + 1] ?? "";
        index += 1;
        continue;
      }
      if (char === inString) {
        inString = null;
      }
      continue;
    }
    if (char === '"' || char === "'") {
      inString = char;
      current += char;
      continue;
    }
    if (char === "(") {
      depth += 1;
      current += char;
      continue;
    }
    if (char === ")") {
      depth = Math.max(0, depth - 1);
      current += char;
      continue;
    }
    if (char === "," && depth === 0) {
      args.push(current.trim());
      current = "";
      continue;
    }
    current += char;
  }
  if (current.trim().length > 0) {
    args.push(current.trim());
  }
  return args;
}
function resolveArgument(raw, ctx) {
  const trimmed = raw.trim();
  if (trimmed.startsWith("${") && trimmed.endsWith("}")) {
    return resolveExpression(trimmed.slice(2, -1), ctx);
  }
  if (trimmed.startsWith("/") || trimmed.match(/^[a-zA-Z_]/)) {
    const resolved = resolveExpression(trimmed, ctx);
    if (resolved !== void 0) {
      return resolved;
    }
  }
  if (trimmed.startsWith('"') && trimmed.endsWith('"') || trimmed.startsWith("'") && trimmed.endsWith("'")) {
    return unquote(trimmed);
  }
  if (trimmed === "true") {
    return true;
  }
  if (trimmed === "false") {
    return false;
  }
  if (trimmed === "null") {
    return null;
  }
  if (trimmed.length === 0) {
    return void 0;
  }
  const numeric = Number(trimmed);
  if (!Number.isNaN(numeric)) {
    return numeric;
  }
  return trimmed;
}
function stringifyValue(value) {
  if (value === null || typeof value === "undefined") {
    return "";
  }
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return JSON.stringify(value);
}
function toNumber(value) {
  if (typeof value === "number") {
    return value;
  }
  const parsed = Number(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}
function unquote(value) {
  const quote = value[0];
  const body = value.slice(1, -1);
  return body.replace(new RegExp(`\\\\${quote}`, "g"), quote).replace(/\\\\/g, "\\");
}

// src/a2ui/events.ts
function buildActionEvent(action, surfaceId, sourceComponentId, options) {
  if (!action?.event?.name) {
    return null;
  }
  const context = resolveActionContext(action.event.context ?? {}, options);
  return {
    action: {
      name: action.event.name,
      surfaceId,
      sourceComponentId,
      timestamp: (options.timestamp ?? /* @__PURE__ */ new Date()).toISOString(),
      context
    }
  };
}
function resolveActionContext(context, options) {
  const resolved = {};
  for (const [key, value] of Object.entries(context)) {
    resolved[key] = resolveDynamicValue(
      value,
      options.dataModel,
      options.scope,
      options.functions
    );
  }
  return resolved;
}

// src/a2ui/renderer.tsx
import { Fragment as Fragment2, jsx as jsx2, jsxs as jsxs2 } from "react/jsx-runtime";
var IconMap2 = {
  "alert-circle": AlertCircle2,
  "check-circle": CheckCircle2,
  "info": Info2,
  "x-circle": XCircle2,
  "user": User2,
  "mail": Mail2,
  "calendar": Calendar2
};
var A2uiRenderContext = createContext2(null);
var A2uiSurfaceRenderer = ({
  store,
  surfaceId,
  rootId = "root",
  onAction,
  theme,
  functions
}) => {
  const surface = store.getSurface(surfaceId);
  const [version, setVersion] = useState2(0);
  const bumpVersion = React2.useCallback(() => {
    setVersion((prev) => prev + 1);
  }, []);
  if (!surface) {
    return null;
  }
  const dataModel = surface.dataModel ?? {};
  const isDark = theme === "dark";
  return /* @__PURE__ */ jsx2(
    A2uiRenderContext.Provider,
    {
      value: {
        store,
        surfaceId,
        dataModel,
        onAction,
        functions,
        bumpVersion
      },
      children: /* @__PURE__ */ jsx2("div", { className: isDark ? "dark" : "", "data-version": version, children: /* @__PURE__ */ jsx2(A2uiComponentRenderer, { componentId: rootId }) })
    }
  );
};
var A2uiComponentRenderer = ({
  componentId,
  scope
}) => {
  const ctx = useContext2(A2uiRenderContext);
  if (!ctx) {
    return null;
  }
  const surface = ctx.store.getSurface(ctx.surfaceId);
  const component = surface?.components.get(componentId);
  if (!component) {
    return null;
  }
  return /* @__PURE__ */ jsx2(
    A2uiComponentView,
    {
      component,
      scope
    }
  );
};
var A2uiComponentView = ({
  component,
  scope
}) => {
  const ctx = useContext2(A2uiRenderContext);
  if (!ctx) {
    return null;
  }
  const resolveString = (value) => resolveDynamicString(value, ctx.dataModel, scope, ctx.functions);
  const resolveValue = (value) => resolveDynamicValue(value, ctx.dataModel, scope, ctx.functions);
  const renderChildList = (children) => {
    if (!children) return null;
    if (Array.isArray(children)) {
      return children.map((childId) => /* @__PURE__ */ jsx2(A2uiComponentRenderer, { componentId: childId, scope }, childId));
    }
    const items = resolvePath(ctx.dataModel, children.path, scope);
    if (!Array.isArray(items)) {
      return null;
    }
    return items.map((item, index) => {
      const itemScope = typeof item === "object" && item !== null ? item : {};
      const key = itemScope && "id" in itemScope && typeof itemScope.id === "string" ? itemScope.id : `${children.componentId}-${index}`;
      return /* @__PURE__ */ jsx2(
        A2uiComponentRenderer,
        {
          componentId: children.componentId,
          scope: itemScope
        },
        key
      );
    });
  };
  const baseComponent = component.component;
  switch (baseComponent) {
    case "Text": {
      const text = resolveString(component.text);
      const variant = component.variant;
      if (variant === "body" || !variant) {
        return /* @__PURE__ */ jsx2("div", { className: "prose prose-sm dark:prose-invert max-w-none text-gray-700 dark:text-gray-300", children: /* @__PURE__ */ jsx2(Markdown2, { children: text }) });
      }
      const Tag = variant === "h1" ? "h1" : variant === "h2" ? "h2" : variant === "h3" ? "h3" : variant === "h4" ? "h4" : variant === "code" ? "code" : "p";
      const classes = clsx2({
        "text-4xl font-bold mb-4 dark:text-white": variant === "h1",
        "text-3xl font-bold mb-3 dark:text-white": variant === "h2",
        "text-2xl font-bold mb-2 dark:text-white": variant === "h3",
        "text-xl font-bold mb-2 dark:text-white": variant === "h4",
        "font-mono bg-gray-100 dark:bg-gray-800 p-1 rounded dark:text-gray-100": variant === "code",
        "text-sm text-gray-500 dark:text-gray-400": variant === "caption"
      });
      return /* @__PURE__ */ jsx2(Tag, { className: classes, children: text });
    }
    case "Image": {
      const url = resolveString(component.url);
      const alt = resolveString(component.alt ?? "");
      const fit = component.fit;
      const style = fit ? { objectFit: fit } : void 0;
      return /* @__PURE__ */ jsx2("img", { src: url, alt, style, className: "max-w-full h-auto" });
    }
    case "Icon": {
      const name = String(component.name ?? "info");
      const Icon = IconMap2[name] || Info2;
      const size = typeof component.size === "number" ? component.size : 24;
      return /* @__PURE__ */ jsx2(Icon, { size });
    }
    case "Row":
    case "Column": {
      const justify = component.justify;
      const align = component.align;
      const flexDirection = baseComponent === "Row" ? "row" : "column";
      const style = {
        display: "flex",
        flexDirection,
        justifyContent: mapJustify(justify),
        alignItems: mapAlign(align),
        gap: 12
      };
      return /* @__PURE__ */ jsx2("div", { style, children: renderChildList(component.children) });
    }
    case "List": {
      const direction = component.direction;
      const style = {
        display: "flex",
        flexDirection: direction === "horizontal" ? "row" : "column",
        gap: 12
      };
      return /* @__PURE__ */ jsx2("div", { style, children: renderChildList(component.children) });
    }
    case "Card": {
      const childId = component.child;
      return /* @__PURE__ */ jsx2("div", { className: "bg-white dark:bg-gray-900 rounded-lg border dark:border-gray-700 shadow-sm overflow-hidden mb-4 p-4", children: /* @__PURE__ */ jsx2(A2uiComponentRenderer, { componentId: childId, scope }) });
    }
    case "Divider": {
      const axis = component.axis;
      return axis === "vertical" ? /* @__PURE__ */ jsx2("div", { className: "w-px bg-gray-200 dark:bg-gray-700 self-stretch mx-2" }) : /* @__PURE__ */ jsx2("div", { className: "h-px bg-gray-200 dark:bg-gray-700 w-full my-2" });
    }
    case "Tabs": {
      const tabs = component.tabs ?? [];
      return /* @__PURE__ */ jsx2(
        A2uiTabs,
        {
          tabs,
          scope
        }
      );
    }
    case "Modal": {
      const triggerId = component.trigger;
      const contentId = component.content;
      return /* @__PURE__ */ jsx2(
        A2uiModal,
        {
          triggerId,
          contentId,
          scope
        }
      );
    }
    case "Button": {
      const childId = component.child;
      const variant = component.variant;
      const action = component.action;
      const btnClasses = clsx2("px-4 py-2 rounded font-medium transition-colors", {
        "bg-blue-600 text-white hover:bg-blue-700": variant === "primary" || !variant,
        "bg-transparent text-blue-600 hover:text-blue-700": variant === "borderless"
      });
      return /* @__PURE__ */ jsx2(
        "button",
        {
          type: "button",
          className: btnClasses,
          onClick: () => {
            const event = buildActionEvent(action, ctx.surfaceId, component.id, {
              dataModel: ctx.dataModel,
              scope,
              functions: ctx.functions
            });
            if (event) {
              ctx.onAction?.(event);
            }
          },
          children: /* @__PURE__ */ jsx2(A2uiComponentRenderer, { componentId: childId, scope })
        }
      );
    }
    case "CheckBox": {
      const label = resolveString(component.label);
      const value = Boolean(resolveValue(component.value));
      const bindingPath = isDataBinding(component.value) ? component.value.path : void 0;
      return /* @__PURE__ */ jsxs2("label", { className: "mb-3 flex items-center gap-2 text-sm font-medium text-gray-700 dark:text-gray-300", children: [
        /* @__PURE__ */ jsx2(
          "input",
          {
            type: "checkbox",
            checked: value,
            onChange: (event) => {
              if (bindingPath) {
                ctx.store.applyUpdateDataModel(ctx.surfaceId, bindingPath, event.currentTarget.checked);
                ctx.bumpVersion();
              }
            },
            className: "h-4 w-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
          }
        ),
        label
      ] });
    }
    case "TextField": {
      const label = resolveString(component.label);
      const variant = component.variant;
      const bindingPath = isDataBinding(component.value) ? component.value.path : void 0;
      const resolved = resolveValue(component.value);
      const value = typeof resolved === "string" ? resolved : resolved ?? "";
      if (variant === "longText") {
        return /* @__PURE__ */ jsxs2("div", { className: "mb-3", children: [
          /* @__PURE__ */ jsx2("label", { className: "block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1", children: label }),
          /* @__PURE__ */ jsx2(
            "textarea",
            {
              value: String(value),
              onChange: (event) => {
                if (bindingPath) {
                  ctx.store.applyUpdateDataModel(ctx.surfaceId, bindingPath, event.currentTarget.value);
                  ctx.bumpVersion();
                }
              },
              className: "w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none bg-white dark:bg-gray-800 dark:border-gray-600 dark:text-white",
              rows: 4
            }
          )
        ] });
      }
      const inputType = variant === "obscured" ? "password" : variant === "number" ? "number" : "text";
      return /* @__PURE__ */ jsxs2("div", { className: "mb-3", children: [
        /* @__PURE__ */ jsx2("label", { className: "block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1", children: label }),
        /* @__PURE__ */ jsx2(
          "input",
          {
            type: inputType,
            value: String(value),
            onChange: (event) => {
              if (bindingPath) {
                const nextValue = inputType === "number" ? event.currentTarget.valueAsNumber : event.currentTarget.value;
                ctx.store.applyUpdateDataModel(ctx.surfaceId, bindingPath, Number.isNaN(nextValue) ? event.currentTarget.value : nextValue);
                ctx.bumpVersion();
              }
            },
            className: "w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none bg-white dark:bg-gray-800 dark:border-gray-600 dark:text-white"
          }
        )
      ] });
    }
    case "ChoicePicker": {
      const label = resolveString(component.label ?? "");
      const options = component.options ?? [];
      const variant = component.variant;
      const bindingPath = isDataBinding(component.value) ? component.value.path : void 0;
      const resolved = resolveValue(component.value);
      const values = Array.isArray(resolved) ? resolved.map(String) : [];
      if (variant === "mutuallyExclusive") {
        return /* @__PURE__ */ jsxs2("div", { className: "mb-3", children: [
          label && /* @__PURE__ */ jsx2("label", { className: "block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1", children: label }),
          /* @__PURE__ */ jsxs2(
            "select",
            {
              value: values[0] ?? "",
              onChange: (event) => {
                if (bindingPath) {
                  ctx.store.applyUpdateDataModel(ctx.surfaceId, bindingPath, [event.currentTarget.value]);
                  ctx.bumpVersion();
                }
              },
              className: "w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none bg-white dark:bg-gray-800 dark:border-gray-600 dark:text-white",
              children: [
                /* @__PURE__ */ jsx2("option", { value: "", children: "Select..." }),
                options.map((opt, i) => /* @__PURE__ */ jsx2("option", { value: opt.value, children: resolveString(opt.label) }, i))
              ]
            }
          )
        ] });
      }
      return /* @__PURE__ */ jsxs2("div", { className: "mb-3", children: [
        label && /* @__PURE__ */ jsx2("label", { className: "block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1", children: label }),
        /* @__PURE__ */ jsx2(
          "select",
          {
            multiple: true,
            value: values,
            onChange: (event) => {
              if (bindingPath) {
                const selected = Array.from(event.currentTarget.selectedOptions).map((opt) => opt.value);
                ctx.store.applyUpdateDataModel(ctx.surfaceId, bindingPath, selected);
                ctx.bumpVersion();
              }
            },
            className: "w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none bg-white dark:bg-gray-800 dark:border-gray-600 dark:text-white",
            children: options.map((opt, i) => /* @__PURE__ */ jsx2("option", { value: opt.value, children: resolveString(opt.label) }, i))
          }
        )
      ] });
    }
    case "Slider": {
      const label = resolveString(component.label ?? "");
      const bindingPath = isDataBinding(component.value) ? component.value.path : void 0;
      const resolved = resolveValue(component.value);
      const value = typeof resolved === "number" ? resolved : Number(resolved ?? 0);
      const min = typeof component.min === "number" ? component.min : 0;
      const max = typeof component.max === "number" ? component.max : 100;
      return /* @__PURE__ */ jsxs2("div", { className: "mb-3", children: [
        label && /* @__PURE__ */ jsx2("label", { className: "block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1", children: label }),
        /* @__PURE__ */ jsx2(
          "input",
          {
            type: "range",
            min,
            max,
            value: Number.isNaN(value) ? min : value,
            onChange: (event) => {
              if (bindingPath) {
                ctx.store.applyUpdateDataModel(ctx.surfaceId, bindingPath, event.currentTarget.valueAsNumber);
                ctx.bumpVersion();
              }
            },
            className: "w-full h-2 bg-gray-200 rounded-lg appearance-none cursor-pointer"
          }
        )
      ] });
    }
    case "DateTimeInput": {
      const label = resolveString(component.label ?? "");
      const bindingPath = isDataBinding(component.value) ? component.value.path : void 0;
      const resolved = resolveValue(component.value);
      const value = typeof resolved === "string" ? resolved : "";
      const enableDate = component.enableDate !== false;
      const enableTime = component.enableTime !== false;
      const inputType = enableDate && enableTime ? "datetime-local" : enableDate ? "date" : "time";
      return /* @__PURE__ */ jsxs2("div", { className: "mb-3", children: [
        label && /* @__PURE__ */ jsx2("label", { className: "block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1", children: label }),
        /* @__PURE__ */ jsx2(
          "input",
          {
            type: inputType,
            value,
            onChange: (event) => {
              if (bindingPath) {
                ctx.store.applyUpdateDataModel(ctx.surfaceId, bindingPath, event.currentTarget.value);
                ctx.bumpVersion();
              }
            },
            className: "w-full px-3 py-2 border rounded-md focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none bg-white dark:bg-gray-800 dark:border-gray-600 dark:text-white"
          }
        )
      ] });
    }
    case "Video": {
      const url = resolveString(component.url);
      return /* @__PURE__ */ jsx2("video", { src: url, controls: true, className: "w-full rounded-md" });
    }
    case "AudioPlayer": {
      const url = resolveString(component.url);
      return /* @__PURE__ */ jsx2("audio", { src: url, controls: true, className: "w-full" });
    }
    default:
      return null;
  }
};
var A2uiModal = ({
  triggerId,
  contentId,
  scope
}) => {
  const [open, setOpen] = useState2(false);
  return /* @__PURE__ */ jsxs2(Fragment2, { children: [
    /* @__PURE__ */ jsx2("span", { onClick: () => setOpen(true), className: "inline-block cursor-pointer", children: /* @__PURE__ */ jsx2(A2uiComponentRenderer, { componentId: triggerId, scope }) }),
    open && /* @__PURE__ */ jsx2("div", { className: "fixed inset-0 bg-black/50 flex items-center justify-center p-4 z-50", children: /* @__PURE__ */ jsxs2("div", { className: "bg-white dark:bg-gray-900 rounded-lg shadow-lg max-w-lg w-full", children: [
      /* @__PURE__ */ jsxs2("div", { className: "p-4 flex items-center justify-between border-b dark:border-gray-700", children: [
        /* @__PURE__ */ jsx2("div", { className: "text-sm font-medium text-gray-700 dark:text-gray-200", children: "Modal" }),
        /* @__PURE__ */ jsx2(
          "button",
          {
            onClick: () => setOpen(false),
            className: "text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200",
            type: "button",
            children: /* @__PURE__ */ jsx2(XCircle2, { className: "w-5 h-5" })
          }
        )
      ] }),
      /* @__PURE__ */ jsx2("div", { className: "p-4", children: /* @__PURE__ */ jsx2(A2uiComponentRenderer, { componentId: contentId, scope }) })
    ] }) })
  ] });
};
var A2uiTabs = ({
  tabs,
  scope
}) => {
  const ctx = useContext2(A2uiRenderContext);
  const [activeTab, setActiveTab] = useState2(0);
  if (!ctx) {
    return null;
  }
  const resolveString = (value) => resolveDynamicString(value, ctx.dataModel, scope, ctx.functions);
  return /* @__PURE__ */ jsxs2("div", { className: "mb-4", children: [
    /* @__PURE__ */ jsx2("div", { className: "border-b border-gray-200", children: /* @__PURE__ */ jsx2("nav", { className: "flex space-x-4", children: tabs.map((tab, i) => /* @__PURE__ */ jsx2(
      "button",
      {
        onClick: () => setActiveTab(i),
        className: clsx2("px-4 py-2 border-b-2 font-medium text-sm transition-colors", {
          "border-blue-600 text-blue-600": activeTab === i,
          "border-transparent text-gray-500 hover:text-gray-700": activeTab !== i
        }),
        children: resolveString(tab.title)
      },
      i
    )) }) }),
    /* @__PURE__ */ jsx2("div", { className: "p-4", children: tabs[activeTab]?.child && /* @__PURE__ */ jsx2(A2uiComponentRenderer, { componentId: tabs[activeTab].child, scope }) })
  ] });
};
function mapJustify(value) {
  switch (value) {
    case "center":
      return "center";
    case "end":
      return "flex-end";
    case "spaceAround":
      return "space-around";
    case "spaceBetween":
      return "space-between";
    case "spaceEvenly":
      return "space-evenly";
    case "stretch":
      return "stretch";
    case "start":
    default:
      return "flex-start";
  }
}
function mapAlign(value) {
  switch (value) {
    case "center":
      return "center";
    case "end":
      return "flex-end";
    case "stretch":
      return "stretch";
    case "start":
    default:
      return "flex-start";
  }
}

// src/types.ts
function uiEventToMessage(event) {
  switch (event.action) {
    case "form_submit":
      return `[UI Event: Form submitted]
Action: ${event.action_id}
Data:
${JSON.stringify(event.data, null, 2)}`;
    case "button_click":
      return `[UI Event: Button clicked]
Action: ${event.action_id}`;
    case "input_change":
      return `[UI Event: Input changed]
Field: ${event.name}
Value: ${event.value}`;
    case "tab_change":
      return `[UI Event: Tab changed]
Index: ${event.index}`;
  }
}

// src/a2ui/store.ts
var A2uiStore = class {
  constructor() {
    __publicField(this, "surfaces", /* @__PURE__ */ new Map());
  }
  getSurface(surfaceId) {
    return this.surfaces.get(surfaceId);
  }
  ensureSurface(surfaceId) {
    const existing = this.surfaces.get(surfaceId);
    if (existing) {
      return existing;
    }
    const created = {
      components: /* @__PURE__ */ new Map(),
      dataModel: {}
    };
    this.surfaces.set(surfaceId, created);
    return created;
  }
  applyUpdateComponents(surfaceId, components) {
    const surface = this.ensureSurface(surfaceId);
    const FORBIDDEN_KEYS = /* @__PURE__ */ new Set(["__proto__", "constructor", "prototype"]);
    for (const component of components) {
      if (!component.id || FORBIDDEN_KEYS.has(component.id)) {
        continue;
      }
      surface.components.set(component.id, component);
    }
  }
  removeSurface(surfaceId) {
    this.surfaces.delete(surfaceId);
  }
  applyUpdateDataModel(surfaceId, path, value) {
    const surface = this.ensureSurface(surfaceId);
    if (!path || path === "/") {
      surface.dataModel = value ?? {};
      return;
    }
    const tokens = path.split("/").filter(Boolean);
    if (tokens.length === 0) {
      surface.dataModel = value ?? {};
      return;
    }
    const FORBIDDEN_KEYS = /* @__PURE__ */ new Set(["__proto__", "constructor", "prototype"]);
    function isSafeKey(k) {
      return !FORBIDDEN_KEYS.has(k);
    }
    let cursor = surface.dataModel;
    for (let i = 0; i < tokens.length - 1; i += 1) {
      const key = tokens[i];
      if (!isSafeKey(key)) {
        return;
      }
      const next = Object.prototype.hasOwnProperty.call(cursor, key) ? cursor[key] : void 0;
      if (typeof next === "object" && next !== null) {
        cursor = next;
      } else {
        const created = /* @__PURE__ */ Object.create(null);
        Object.defineProperty(cursor, key, { value: created, writable: true, enumerable: true, configurable: true });
        cursor = created;
      }
    }
    const lastKey = tokens[tokens.length - 1];
    if (!isSafeKey(lastKey)) {
      return;
    }
    if (typeof value === "undefined") {
      delete cursor[lastKey];
    } else {
      Object.defineProperty(cursor, lastKey, { value, writable: true, enumerable: true, configurable: true });
    }
  }
};

// src/a2ui/parser.ts
function parseJsonl(payload) {
  return payload.split("\n").map((line) => line.trim()).filter((line) => line.length > 0).map((line) => ({
    message: JSON.parse(line),
    raw: line
  }));
}
function applyParsedMessages(store, parsed) {
  for (const entry of parsed) {
    const message = entry.message;
    if ("createSurface" in message) {
      store.ensureSurface(message.createSurface.surfaceId);
    } else if ("updateComponents" in message) {
      store.applyUpdateComponents(
        message.updateComponents.surfaceId,
        message.updateComponents.components
      );
    } else if ("updateDataModel" in message) {
      store.applyUpdateDataModel(
        message.updateDataModel.surfaceId,
        message.updateDataModel.path,
        message.updateDataModel.value
      );
    } else if ("deleteSurface" in message) {
      store.removeSurface(message.deleteSurface.surfaceId);
    }
  }
}

// src/protocols.ts
function isRecord(value) {
  return typeof value === "object" && value !== null;
}
function getString(value) {
  return typeof value === "string" ? value : void 0;
}
function surfaceToJsonl(surface) {
  const messages = [
    {
      createSurface: {
        surfaceId: surface.surfaceId,
        catalogId: surface.catalogId,
        theme: surface.theme ?? void 0,
        sendDataModel: surface.sendDataModel ?? true
      }
    }
  ];
  if (surface.dataModel) {
    messages.push({
      updateDataModel: {
        surfaceId: surface.surfaceId,
        path: "/",
        value: surface.dataModel
      }
    });
  }
  messages.push({
    updateComponents: {
      surfaceId: surface.surfaceId,
      components: surface.components
    }
  });
  return `${messages.map((entry) => JSON.stringify(entry)).join("\n")}
`;
}
function extractSurfaceFromAgUiEvents(events) {
  for (const event of events) {
    if (getString(event.type) !== "CUSTOM") {
      continue;
    }
    if (getString(event.name) !== "adk.ui.surface") {
      continue;
    }
    const value = event.value;
    if (!isRecord(value)) {
      continue;
    }
    const surface = value.surface;
    if (!isRecord(surface)) {
      continue;
    }
    const surfaceId = getString(surface.surfaceId);
    const catalogId = getString(surface.catalogId);
    const components = Array.isArray(surface.components) ? surface.components.filter((entry) => isRecord(entry)) : [];
    if (!surfaceId || !catalogId || components.length === 0) {
      continue;
    }
    return {
      surfaceId,
      catalogId,
      components,
      dataModel: isRecord(surface.dataModel) ? surface.dataModel : void 0,
      theme: isRecord(surface.theme) ? surface.theme : void 0,
      sendDataModel: typeof surface.sendDataModel === "boolean" ? surface.sendDataModel : void 0
    };
  }
  return null;
}
function extractSurfaceScriptFromHtml(html) {
  const openTagStart = html.indexOf("<script");
  if (openTagStart === -1) return null;
  const idAttr = html.indexOf("adk-ui-surface", openTagStart);
  if (idAttr === -1) return null;
  const openTagEnd = html.indexOf(">", idAttr);
  if (openTagEnd === -1) return null;
  const closeTag = html.indexOf("</script>", openTagEnd);
  if (closeTag === -1) return null;
  const content = html.substring(openTagEnd + 1, closeTag).trim();
  return content.length > 0 ? content : null;
}
function extractSurfaceFromMcpPayload(payload) {
  const resourceReadResponse = payload.resourceReadResponse;
  if (!isRecord(resourceReadResponse)) {
    return null;
  }
  const contents = resourceReadResponse.contents;
  if (!Array.isArray(contents) || contents.length === 0) {
    return null;
  }
  const firstContent = contents[0];
  if (!isRecord(firstContent)) {
    return null;
  }
  const html = getString(firstContent.text);
  if (!html) {
    return null;
  }
  const scriptText = extractSurfaceScriptFromHtml(html);
  if (!scriptText) {
    return null;
  }
  const parsed = JSON.parse(scriptText);
  if (!isRecord(parsed)) {
    return null;
  }
  const surfaceId = getString(parsed.surfaceId);
  const catalogId = getString(parsed.catalogId);
  const components = Array.isArray(parsed.components) ? parsed.components.filter((entry) => isRecord(entry)) : [];
  if (!surfaceId || !catalogId || components.length === 0) {
    return null;
  }
  return {
    surfaceId,
    catalogId,
    components,
    dataModel: isRecord(parsed.dataModel) ? parsed.dataModel : void 0,
    theme: isRecord(parsed.theme) ? parsed.theme : void 0,
    sendDataModel: typeof parsed.sendDataModel === "boolean" ? parsed.sendDataModel : void 0
  };
}
function protocolEnvelopeToJsonl(envelope) {
  if (typeof envelope.jsonl === "string") {
    return envelope.jsonl;
  }
  const protocol = getString(envelope.protocol);
  if (protocol === "ag_ui" && Array.isArray(envelope.events)) {
    const surface = extractSurfaceFromAgUiEvents(
      envelope.events.filter((entry) => isRecord(entry))
    );
    if (!surface) {
      return null;
    }
    return surfaceToJsonl(surface);
  }
  if (protocol === "mcp_apps" && isRecord(envelope.payload)) {
    const surface = extractSurfaceFromMcpPayload(envelope.payload);
    if (!surface) {
      return null;
    }
    return surfaceToJsonl(surface);
  }
  return null;
}
function parseProtocolPayload(payload) {
  if (typeof payload === "string") {
    return parseJsonl(payload);
  }
  if (!isRecord(payload)) {
    return [];
  }
  const maybeEnvelope = payload;
  const jsonl = protocolEnvelopeToJsonl(maybeEnvelope);
  if (!jsonl) {
    return [];
  }
  return parseJsonl(jsonl);
}
function applyProtocolPayload(store, payload) {
  const parsed = parseProtocolPayload(payload);
  if (parsed.length > 0) {
    applyParsedMessages(store, parsed);
  }
  return parsed;
}

// src/store.ts
function isRecord2(value) {
  return typeof value === "object" && value !== null;
}
function isLegacyComponentArray(value) {
  return Array.isArray(value) && value.every((entry) => isRecord2(entry) && typeof entry.type === "string");
}
function getUiTheme(value) {
  if (value === "light" || value === "dark" || value === "system") {
    return value;
  }
  return void 0;
}
function extractLegacyUiResponse(payload) {
  if (!isRecord2(payload)) {
    return null;
  }
  if (isLegacyComponentArray(payload.components)) {
    return {
      id: typeof payload.id === "string" ? payload.id : void 0,
      theme: getUiTheme(payload.theme),
      components: payload.components
    };
  }
  if (isRecord2(payload.payload) && isLegacyComponentArray(payload.payload.components)) {
    return {
      id: typeof payload.payload.id === "string" ? payload.payload.id : void 0,
      theme: getUiTheme(payload.payload.theme),
      components: payload.payload.components
    };
  }
  return null;
}
var UnifiedRenderStore = class {
  constructor(a2uiStore = new A2uiStore()) {
    __publicField(this, "a2uiStore");
    __publicField(this, "legacyUiResponse", null);
    this.a2uiStore = a2uiStore;
  }
  getA2uiStore() {
    return this.a2uiStore;
  }
  getLegacyUiResponse() {
    return this.legacyUiResponse;
  }
  clearLegacyUiResponse() {
    this.legacyUiResponse = null;
  }
  applyPayload(payload) {
    const parsed = applyProtocolPayload(this.a2uiStore, payload);
    if (parsed.length > 0) {
      return parsed;
    }
    const legacy = extractLegacyUiResponse(payload);
    if (legacy) {
      this.legacyUiResponse = legacy;
    }
    return [];
  }
};

// src/client.ts
var DEFAULT_SURFACE_ID = "main";
function buildOutboundEvent(protocol, event, options = {}) {
  const surfaceId = options.surfaceId ?? DEFAULT_SURFACE_ID;
  switch (protocol) {
    case "ag_ui":
      return {
        protocol: "ag_ui",
        event: {
          type: "CUSTOM",
          name: "adk.ui.event",
          threadId: options.threadId ?? `thread-${surfaceId}`,
          runId: options.runId ?? `run-${surfaceId}`,
          value: {
            surfaceId,
            event
          }
        }
      };
    case "mcp_apps":
      return {
        protocol: "mcp_apps",
        method: "ui.event",
        params: {
          surfaceId,
          event
        }
      };
    case "a2ui":
    case "adk_ui":
    default:
      return {
        protocol,
        event: {
          surfaceId,
          ...event
        }
      };
  }
}
var ProtocolClient = class {
  constructor(options = {}) {
    __publicField(this, "protocol");
    __publicField(this, "store");
    this.protocol = options.protocol ?? "adk_ui";
    this.store = options.store ?? new UnifiedRenderStore();
  }
  getProtocol() {
    return this.protocol;
  }
  setProtocol(protocol) {
    this.protocol = protocol;
  }
  getStore() {
    return this.store;
  }
  applyPayload(payload) {
    return this.store.applyPayload(payload);
  }
  buildOutboundEvent(event, options = {}) {
    return buildOutboundEvent(this.protocol, event, options);
  }
};
function createProtocolClient(options = {}) {
  return new ProtocolClient(options);
}
export {
  A2uiStore,
  A2uiSurfaceRenderer,
  ProtocolClient,
  Renderer,
  StreamingRenderer,
  UnifiedRenderStore,
  applyParsedMessages,
  applyProtocolPayload,
  applyUiUpdate,
  applyUiUpdates,
  buildActionEvent,
  buildOutboundEvent,
  createProtocolClient,
  isDataBinding,
  isFunctionCall,
  parseJsonl,
  parseProtocolPayload,
  resolveDynamicString,
  resolveDynamicValue,
  resolvePath,
  uiEventToMessage
};
