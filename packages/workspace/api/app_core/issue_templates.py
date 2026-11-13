"""Workspace-owned work forms; business definitions are installed data."""
from datetime import date
from decimal import Decimal
import re

from pydantic import Field, ValidationError, model_validator

from .hosted_operations import HostedOperationError
from .http.schema import StrictSchema
from .models import WorkspaceIssueTemplate


class TemplateField(StrictSchema):
    key: str = Field(pattern=r"^[a-z][A-Za-z0-9]{0,63}$")
    label: str = Field(min_length=1, max_length=160)
    type: str
    required: bool = False
    min_value: str | None = None
    fields: list["TemplateField"] = Field(default_factory=list, max_length=16)

    @model_validator(mode="after")
    def valid_type(self):
        if self.type not in {"text", "textarea", "date", "money", "items"}:
            raise ValueError("template_field_type_invalid")
        if (self.type == "items") != bool(self.fields) or any(f.type == "items" for f in self.fields):
            raise ValueError("template_field_children_invalid")
        if len({f.key for f in self.fields}) != len(self.fields):
            raise ValueError("template_field_duplicate")
        if self.min_value is not None and (self.type != "money" or not re.fullmatch(r"\d+(\.\d{1,2})?", self.min_value)):
            raise ValueError("template_minimum_invalid")
        return self


class TemplateDefinition(StrictSchema):
    visibility: str = "participants"
    instructions: str = Field(min_length=1, max_length=8192)
    fields: list[TemplateField] = Field(min_length=1, max_length=24)
    date_order: list[list[str]] = Field(default_factory=list, max_length=8)

    @model_validator(mode="after")
    def valid_definition(self):
        if self.visibility not in {"workspace", "participants"}:
            raise ValueError("template_visibility_invalid")
        by_key = {f.key: f for f in self.fields}
        if len(by_key) != len(self.fields):
            raise ValueError("template_field_duplicate")
        if any(len(pair) != 2 or any(k not in by_key or by_key[k].type != "date" for k in pair) for pair in self.date_order):
            raise ValueError("template_date_order_invalid")
        return self


def validate_definition(value):
    try:
        return TemplateDefinition.model_validate(value)
    except ValidationError as error:
        raise HostedOperationError(400, "issue_template_invalid") from error


def validate_properties(definition, properties):
    def fields_value(fields, values):
        if not isinstance(values, dict) or set(values) - {f.key for f in fields}:
            raise HostedOperationError(400, "issue_properties_invalid")
        result = {}
        for field in fields:
            value = values.get(field.key)
            if value in (None, "", []):
                if field.required:
                    raise HostedOperationError(400, "issue_properties_required")
                continue
            if field.type == "items":
                if not isinstance(value, list) or not 1 <= len(value) <= 100:
                    raise HostedOperationError(400, "issue_properties_invalid")
                result[field.key] = [fields_value(field.fields, row) for row in value]
                continue
            if not isinstance(value, str) or not value.strip() or len(value) > (8192 if field.type == "textarea" else 512):
                raise HostedOperationError(400, "issue_properties_invalid")
            value = value.strip()
            if field.type == "money":
                if not re.fullmatch(r"(0|[1-9]\d{0,11})(\.\d{1,2})?", value):
                    raise HostedOperationError(400, "issue_properties_invalid")
                if field.min_value is not None and Decimal(value) < Decimal(field.min_value):
                    raise HostedOperationError(400, "issue_properties_invalid")
                value = format(Decimal(value), ".2f")
            if field.type == "date":
                try:
                    if date.fromisoformat(value).isoformat() != value:
                        raise ValueError
                except ValueError as error:
                    raise HostedOperationError(400, "issue_properties_invalid") from error
            result[field.key] = value
        return result
    result = fields_value(definition.fields, properties)
    for earlier, later in definition.date_order:
        if result.get(earlier) and result.get(later) and result[earlier] > result[later]:
            raise HostedOperationError(400, "issue_properties_date_order")
    return result


def prepare_template(workspace_id, key, properties):
    if not key:
        if properties:
            raise HostedOperationError(400, "issue_properties_without_template")
        return {}, {}, "workspace"
    template = WorkspaceIssueTemplate.objects.filter(workspace_id=workspace_id, key=key, enabled=True).first()
    if template is None:
        raise HostedOperationError(404, "issue_template_not_found")
    definition = validate_definition(template.definition)
    return {"name": template.name, "revision": template.revision,
            "definition": definition.model_dump(by_alias=True)}, validate_properties(definition, properties), definition.visibility


def issue_execution_prompt(issue):
    prompt = f"{issue.title}\n\n{issue.description}".strip()
    if not issue.templateKey:
        return prompt
    definition = validate_definition(issue.templateSnapshot["definition"])
    values = validate_properties(definition, issue.properties)
    lines = []
    for field in definition.fields:
        value = values.get(field.key)
        if value is None:
            continue
        if field.type == "items":
            lines.append(field.label)
            for index, row in enumerate(value, 1):
                lines.append(f"{index}. " + "; ".join(f"{child.label}: {row[child.key]}" for child in field.fields if child.key in row))
        else:
            lines.append(f"{field.label}: {value}")
    return prompt + "\n\n" + "\n".join(lines) + "\n\n" + definition.instructions
