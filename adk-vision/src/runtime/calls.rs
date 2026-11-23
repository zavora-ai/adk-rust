//! The runtime's task methods and escalation.

use super::{
    Backend, CallOptions, CallRecord, Escalation, Inputs, Prepared, Route, Trigger, VisionOutput,
    VisionRuntime, mismatch, planned_size,
};
use crate::backend::{BackendDescriptor, BackendId};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::edit::EditOp;
use crate::error::{Result, VisionError};
use crate::geometry::{
    BBox, EscalatedFrom, EscalationReason, RawRegion, Region, RegionNormalizer, Shape, resample,
};
use crate::manifest::VisionTask;
use crate::media::{ImageRef, Provenance, SourceImage};
use crate::sent::SentImage;
use crate::traits::{
    Block, DescribeRequest, DescribeSpec, Description, DetectQuery, DocumentPage, ReadOptions,
    SegmentPrompt, TableCell, Word,
};

/// An edited image, stored with `Provenance::Derived`.
#[derive(Clone, Debug)]
pub struct Edited {
    /// The stored image.
    pub image: ImageRef,
    /// Its pixels and digest.
    pub source: SourceImage,
}

enum RegionTask<'a> {
    Detect(&'a DetectQuery),
    Segment(&'a [SegmentPrompt]),
}

impl RegionTask<'_> {
    fn task(&self) -> VisionTask {
        match self {
            Self::Detect(_) => VisionTask::Detect,
            Self::Segment(_) => VisionTask::Segment,
        }
    }
}

impl VisionRuntime {
    /// Detects objects in a source image on the detection route.
    ///
    /// # Errors
    ///
    /// The route's, the gate's or the backend's error; see [`detect_with`](Self::detect_with).
    pub async fn detect(
        &self,
        cx: &VisionCx,
        image: &SourceImage,
        query: &DetectQuery,
    ) -> Result<VisionOutput<Vec<Region>>> {
        self.detect_with(cx, image, query, &CallOptions::default()).await
    }

    /// Detects objects with per-call options. Regions come back in source pixels; a route
    /// with an escalation crops each uncertain region, plus margin, and sends only that.
    ///
    /// # Errors
    ///
    /// [`VisionError::BackendNotFound`], the egress gate's refusals for a remote backend,
    /// a budget or kill-switch refusal before the request, [`VisionError::Cancelled`], or
    /// the backend's error.
    pub async fn detect_with(
        &self,
        cx: &VisionCx,
        image: &SourceImage,
        query: &DetectQuery,
        options: &CallOptions,
    ) -> Result<VisionOutput<Vec<Region>>> {
        let route = self.route_for(VisionTask::Detect, options, self.detectors.default_id())?;
        self.regions_routed(cx, &RegionTask::Detect(query), &route, image, options).await
    }

    /// Segments a source image on the segmentation route. Prompts are in source pixels.
    ///
    /// # Errors
    ///
    /// As [`detect_with`](Self::detect_with).
    pub async fn segment(
        &self,
        cx: &VisionCx,
        image: &SourceImage,
        prompts: &[SegmentPrompt],
    ) -> Result<VisionOutput<Vec<Region>>> {
        self.segment_with(cx, image, prompts, &CallOptions::default()).await
    }

    /// Segments with per-call options.
    ///
    /// # Errors
    ///
    /// As [`detect_with`](Self::detect_with), and [`VisionError::Unsupported`] for a mask
    /// prompt to a backend whose frame has no raster.
    pub async fn segment_with(
        &self,
        cx: &VisionCx,
        image: &SourceImage,
        prompts: &[SegmentPrompt],
        options: &CallOptions,
    ) -> Result<VisionOutput<Vec<Region>>> {
        let route = self.route_for(VisionTask::Segment, options, self.segmenters.default_id())?;
        self.regions_routed(cx, &RegionTask::Segment(prompts), &route, image, options).await
    }

    async fn regions_routed(
        &self,
        cx: &VisionCx,
        task: &RegionTask<'_>,
        route: &Route,
        image: &SourceImage,
        options: &CallOptions,
    ) -> Result<VisionOutput<Vec<Region>>> {
        let mut calls = Vec::new();
        let regions = self
            .regions_once(cx, task, &route.primary, image, options.region, options, &mut calls)
            .await?;
        let regions = match &route.escalation {
            Some(escalation) => {
                self.escalate_regions(
                    cx,
                    task,
                    &route.primary,
                    escalation,
                    image,
                    regions,
                    options,
                    &mut calls,
                )
                .await?
            }
            None => regions,
        };
        Ok(VisionOutput { value: regions, calls })
    }

    #[allow(clippy::too_many_arguments)] // One call's full context; a struct would only rename it.
    async fn regions_once(
        &self,
        cx: &VisionCx,
        task: &RegionTask<'_>,
        id: &BackendId,
        image: &SourceImage,
        crop: Option<BBox>,
        options: &CallOptions,
        calls: &mut Vec<CallRecord>,
    ) -> Result<Vec<Region>> {
        let (descriptor, raw, prepared, mut record) = match task {
            RegionTask::Detect(query) => {
                let (descriptor, backend) = self.detectors.get(id)?;
                let sent = planned_size(image.size(), crop, &descriptor.limits);
                let concepts = match query {
                    DetectQuery::Classes(c) | DetectQuery::Concepts(c) => c.len(),
                };
                // The image is processed once per request the backend makes for the query.
                let requests = descriptor.requests_for(concepts, false) as usize;
                let hold = self
                    .preflight(
                        cx,
                        task.task(),
                        descriptor,
                        &vec![sent; requests],
                        0,
                        None,
                        options.image_unit(),
                    )
                    .await?;
                let prepared =
                    self.prepare(cx, descriptor, backend.is_remote(), image, crop, options).await?;
                let work = match (backend, &prepared) {
                    (Backend::Local(b), Prepared::Local(s)) => b.detect(cx, s, query),
                    (Backend::Remote(b), Prepared::Remote(r)) => b.detect(cx, r, query),
                    _ => return Err(mismatch(descriptor)),
                };
                let inputs = Inputs::of(std::slice::from_ref(&prepared));
                let (raw, record) =
                    self.metered(cx, task.task(), descriptor, inputs, hold, work).await?;
                (descriptor, raw, prepared, record)
            }
            RegionTask::Segment(prompts) => {
                let (descriptor, backend) = self.segmenters.get(id)?;
                let sent = planned_size(image.size(), crop, &descriptor.limits);
                let concepts =
                    prompts.iter().filter(|p| matches!(p, SegmentPrompt::Concept { .. })).count();
                // The image is processed once per request the backend makes for the prompts.
                let requests = descriptor.requests_for(concepts, concepts < prompts.len()) as usize;
                let hold = self
                    .preflight(
                        cx,
                        task.task(),
                        descriptor,
                        &vec![sent; requests],
                        0,
                        None,
                        options.image_unit(),
                    )
                    .await?;
                let prepared =
                    self.prepare(cx, descriptor, backend.is_remote(), image, crop, options).await?;
                let prompts = prompts_for(descriptor, prepared.image(), prompts)?;
                let work = match (backend, &prepared) {
                    (Backend::Local(b), Prepared::Local(s)) => b.segment(cx, s, &prompts),
                    (Backend::Remote(b), Prepared::Remote(r)) => b.segment(cx, r, &prompts),
                    _ => return Err(mismatch(descriptor)),
                };
                let inputs = Inputs::of(std::slice::from_ref(&prepared));
                let (raw, record) =
                    self.metered(cx, task.task(), descriptor, inputs, hold, work).await?;
                (descriptor, raw, prepared, record)
            }
        };
        let regions = Self::normalizer(descriptor, &prepared).regions(raw)?;
        record.regions = regions.len();
        calls.push(record);
        Ok(regions)
    }

    #[allow(clippy::too_many_arguments)] // One escalation's full context.
    async fn escalate_regions(
        &self,
        cx: &VisionCx,
        task: &RegionTask<'_>,
        primary: &BackendId,
        escalation: &Escalation,
        image: &SourceImage,
        regions: Vec<Region>,
        options: &CallOptions,
        calls: &mut Vec<CallRecord>,
    ) -> Result<Vec<Region>> {
        let (w, h) = image.size();
        // (index of the primary region it replaces, reason, the box to crop)
        let mut items: Vec<(Option<usize>, EscalationReason, Option<BBox>)> = Vec::new();
        let listed = |items: &[(Option<usize>, EscalationReason, Option<BBox>)], i: usize| {
            items.iter().any(|(j, _, _)| *j == Some(i))
        };
        for trigger in &escalation.triggers {
            match trigger {
                Trigger::LowConfidence { threshold } => {
                    for (i, region) in regions.iter().enumerate() {
                        if region.score.is_some_and(|s| s < *threshold)
                            && !listed(&items, i)
                            && let Some(bbox) = region.shape.bbox()
                        {
                            let reason = EscalationReason::LowConfidence { threshold: *threshold };
                            items.push((Some(i), reason, Some(bbox)));
                        }
                    }
                }
                Trigger::NoResult => {
                    if regions.is_empty()
                        && !items.iter().any(|(_, r, _)| *r == EscalationReason::NoResult)
                    {
                        items.push((None, EscalationReason::NoResult, None));
                    }
                }
                Trigger::Disagreement { with, min_iou } => {
                    let other = self
                        .regions_once(cx, task, with, image, options.region, options, calls)
                        .await?;
                    let agrees = |a: &Region, b: &Region| match (a.shape.bbox(), b.shape.bbox()) {
                        (Some(x), Some(y)) => x.iou(&y) >= *min_iou,
                        _ => false,
                    };
                    let reason = EscalationReason::Disagreement { with: with.clone() };
                    for (i, region) in regions.iter().enumerate() {
                        if !other.iter().any(|o| agrees(region, o)) && !listed(&items, i) {
                            items.push((Some(i), reason.clone(), region.shape.bbox()));
                        }
                    }
                    for o in &other {
                        if !regions.iter().any(|r| agrees(r, o)) {
                            items.push((None, reason.clone(), o.shape.bbox()));
                        }
                    }
                }
            }
        }
        if items.is_empty() {
            return Ok(regions);
        }
        let esc_options = escalation_options(escalation, options);
        let mut replacements: Vec<(usize, Vec<Region>)> = Vec::new();
        let mut additions = Vec::new();
        for (index, reason, bbox) in items {
            let crop = bbox.map(|b| b.expanded(escalation.margin).clamped(w, h));
            let found = match self
                .regions_once(cx, task, &escalation.backend, image, crop, &esc_options, calls)
                .await
            {
                Ok(found) => found,
                Err(err @ VisionError::EgressRefused { .. }) => {
                    escalation_refused(escalation, &err);
                    continue;
                }
                Err(err) => return Err(err),
            };
            if let Some(last) = calls.last_mut() {
                last.escalation = Some(reason.clone());
            }
            let replaced = index.map(|i| &regions[i]);
            let from = EscalatedFrom {
                backend: primary.clone(),
                reason,
                label: replaced.and_then(|r| r.label.clone()),
                score: replaced.and_then(|r| r.score),
                text: None,
            };
            let found: Vec<Region> = found
                .into_iter()
                .map(|mut r| {
                    r.origin.escalated_from = Some(from.clone());
                    r
                })
                .collect();
            match index {
                Some(i) if !found.is_empty() => replacements.push((i, found)),
                Some(_) => {}
                None => additions.extend(found),
            }
        }
        let mut out = Vec::with_capacity(regions.len() + additions.len());
        for (i, region) in regions.into_iter().enumerate() {
            match replacements.iter_mut().find(|(j, _)| *j == i) {
                Some((_, found)) => out.append(found),
                None => out.push(region),
            }
        }
        out.extend(additions);
        Ok(out)
    }

    /// Reads a document page on the reading route.
    ///
    /// # Errors
    ///
    /// As [`detect_with`](Self::detect_with).
    pub async fn read(
        &self,
        cx: &VisionCx,
        page: &SourceImage,
        read: &ReadOptions,
    ) -> Result<VisionOutput<DocumentPage>> {
        self.read_with(cx, page, read, &CallOptions::default()).await
    }

    /// Reads with per-call options. A block below a low-confidence trigger is re-read from
    /// its crop by the escalation backend, which replaces its text with `escalatedFrom`
    /// provenance on its region.
    ///
    /// # Errors
    ///
    /// As [`detect_with`](Self::detect_with).
    pub async fn read_with(
        &self,
        cx: &VisionCx,
        page: &SourceImage,
        read: &ReadOptions,
        options: &CallOptions,
    ) -> Result<VisionOutput<DocumentPage>> {
        let route = self.route_for(VisionTask::Read, options, self.readers.default_id())?;
        let mut calls = Vec::new();
        let mut result = self
            .read_once(cx, &route.primary, page, options.region, read, options, &mut calls)
            .await?;
        let Some(escalation) = &route.escalation else {
            return Ok(VisionOutput { value: result, calls });
        };
        let esc_options = escalation_options(escalation, options);
        let (w, h) = page.size();
        for trigger in &escalation.triggers {
            match trigger {
                Trigger::NoResult if result.blocks.is_empty() => {
                    let mut whole = self
                        .read_once(
                            cx,
                            &escalation.backend,
                            page,
                            options.region,
                            read,
                            &esc_options,
                            &mut calls,
                        )
                        .await?;
                    if let Some(last) = calls.last_mut() {
                        last.escalation = Some(EscalationReason::NoResult);
                    }
                    let from = EscalatedFrom {
                        backend: route.primary.clone(),
                        reason: EscalationReason::NoResult,
                        label: None,
                        score: None,
                        text: None,
                    };
                    for block in &mut whole.blocks {
                        block.region.origin.escalated_from = Some(from.clone());
                    }
                    result = whole;
                }
                Trigger::LowConfidence { threshold } => {
                    for i in 0..result.blocks.len() {
                        let block = &result.blocks[i];
                        let low = block.confidence.is_some_and(|c| c < *threshold);
                        let escalated = block.region.origin.escalated_from.is_some();
                        let Some(bbox) = block.region.shape.bbox().filter(|_| low && !escalated)
                        else {
                            continue;
                        };
                        let crop = bbox.expanded(escalation.margin).clamped(w, h);
                        let sub = match self
                            .read_once(
                                cx,
                                &escalation.backend,
                                page,
                                Some(crop),
                                read,
                                &esc_options,
                                &mut calls,
                            )
                            .await
                        {
                            Ok(sub) => sub,
                            Err(err @ VisionError::EgressRefused { .. }) => {
                                escalation_refused(escalation, &err);
                                continue;
                            }
                            Err(err) => return Err(err),
                        };
                        let reason = EscalationReason::LowConfidence { threshold: *threshold };
                        if let Some(last) = calls.last_mut() {
                            last.escalation = Some(reason.clone());
                        }
                        if sub.blocks.is_empty() {
                            continue;
                        }
                        let block = &mut result.blocks[i];
                        let from = EscalatedFrom {
                            backend: route.primary.clone(),
                            reason,
                            label: None,
                            score: block.confidence,
                            text: Some(block.text.clone()),
                        };
                        let order: Vec<usize> = if sub.reading_order.is_empty() {
                            (0..sub.blocks.len()).collect()
                        } else {
                            sub.reading_order.clone()
                        };
                        block.text = order
                            .iter()
                            .filter_map(|&j| sub.blocks.get(j).map(|b| b.text.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        block.confidence =
                            sub.blocks.iter().filter_map(|b| b.confidence).reduce(f32::max);
                        block.words = sub.blocks.iter().flat_map(|b| b.words.clone()).collect();
                        let mut origin = sub.blocks[0].region.origin.clone();
                        origin.escalated_from = Some(from);
                        block.region.origin = origin;
                    }
                }
                Trigger::NoResult | Trigger::Disagreement { .. } => {}
            }
        }
        Ok(VisionOutput { value: result, calls })
    }

    #[allow(clippy::too_many_arguments)] // One call's full context; a struct would only rename it.
    async fn read_once(
        &self,
        cx: &VisionCx,
        id: &BackendId,
        page: &SourceImage,
        crop: Option<BBox>,
        read: &ReadOptions,
        options: &CallOptions,
        calls: &mut Vec<CallRecord>,
    ) -> Result<DocumentPage> {
        let (descriptor, backend) = self.readers.get(id)?;
        let sent = planned_size(page.size(), crop, &descriptor.limits);
        let hold = self
            .preflight(cx, VisionTask::Read, descriptor, &[sent], 0, None, options.image_unit())
            .await?;
        let prepared =
            self.prepare(cx, descriptor, backend.is_remote(), page, crop, options).await?;
        let work = match (backend, &prepared) {
            (Backend::Local(b), Prepared::Local(s)) => b.read(cx, s, read),
            (Backend::Remote(b), Prepared::Remote(r)) => b.read(cx, r, read),
            _ => return Err(mismatch(descriptor)),
        };
        let inputs = Inputs::of(std::slice::from_ref(&prepared));
        let (raw, mut record) =
            self.metered(cx, VisionTask::Read, descriptor, inputs, hold, work).await?;
        let page = normalize_page(&Self::normalizer(descriptor, &prepared), raw)?;
        record.regions = page.blocks.len();
        calls.push(record);
        Ok(page)
    }

    /// Describes source images on the describing route.
    ///
    /// # Errors
    ///
    /// As [`detect_with`](Self::detect_with).
    pub async fn describe(
        &self,
        cx: &VisionCx,
        images: &[&SourceImage],
        spec: &DescribeSpec,
    ) -> Result<VisionOutput<Description>> {
        self.describe_with(cx, images, spec, &CallOptions::default()).await
    }

    /// Describes with per-call options. On a trigger the escalation backend answers the
    /// same request, and its answer replaces the primary's with `escalatedFrom` provenance.
    ///
    /// # Errors
    ///
    /// As [`detect_with`](Self::detect_with), and [`VisionError::InvalidInput`] for no
    /// image.
    pub async fn describe_with(
        &self,
        cx: &VisionCx,
        images: &[&SourceImage],
        spec: &DescribeSpec,
        options: &CallOptions,
    ) -> Result<VisionOutput<Description>> {
        if images.is_empty() {
            return Err(VisionError::InvalidInput("describe needs at least one image".into()));
        }
        let route = self.route_for(VisionTask::Describe, options, self.describers.default_id())?;
        let mut calls = Vec::new();
        let mut answer =
            self.describe_once(cx, &route.primary, images, spec, options, &mut calls).await?;
        if let Some(escalation) = &route.escalation {
            let reason = escalation.triggers.iter().find_map(|trigger| match trigger {
                Trigger::LowConfidence { threshold }
                    if answer.confidence.is_some_and(|c| c < *threshold) =>
                {
                    Some(EscalationReason::LowConfidence { threshold: *threshold })
                }
                Trigger::NoResult
                    if answer.text.is_none()
                        && answer.value.is_none()
                        && answer.regions.is_empty() =>
                {
                    Some(EscalationReason::NoResult)
                }
                _ => None,
            });
            let esc_options = escalation_options(escalation, options);
            let escalated = match reason {
                Some(reason) => match self
                    .describe_once(cx, &escalation.backend, images, spec, &esc_options, &mut calls)
                    .await
                {
                    Ok(escalated) => Some((reason, escalated)),
                    Err(err @ VisionError::EgressRefused { .. }) => {
                        escalation_refused(escalation, &err);
                        None
                    }
                    Err(err) => return Err(err),
                },
                None => None,
            };
            if let Some((reason, mut escalated)) = escalated {
                if let Some(last) = calls.last_mut() {
                    last.escalation = Some(reason.clone());
                }
                let from = EscalatedFrom {
                    backend: route.primary.clone(),
                    reason,
                    label: None,
                    score: answer.confidence,
                    text: answer.text.clone(),
                };
                for region in &mut escalated.regions {
                    region.origin.escalated_from = Some(from.clone());
                }
                escalated.escalated_from = Some(from);
                answer = escalated;
            }
        }
        Ok(VisionOutput { value: answer, calls })
    }

    async fn describe_once(
        &self,
        cx: &VisionCx,
        id: &BackendId,
        images: &[&SourceImage],
        spec: &DescribeSpec,
        options: &CallOptions,
        calls: &mut Vec<CallRecord>,
    ) -> Result<Description> {
        let (descriptor, backend) = self.describers.get(id)?;
        let sizes: Vec<(u32, u32)> = images
            .iter()
            .map(|i| planned_size(i.size(), options.region, &descriptor.limits))
            .collect();
        let hold = self
            .preflight(
                cx,
                VisionTask::Describe,
                descriptor,
                &sizes,
                spec.prompt.len(),
                spec.max_output_tokens,
                options.image_unit(),
            )
            .await?;
        let prepared =
            self.prepare_all(cx, descriptor, backend.is_remote(), images, options).await?;
        let normalizers: Vec<RegionNormalizer> =
            prepared.iter().map(|p| Self::normalizer(descriptor, p)).collect();
        let inputs = Inputs::of(&prepared);
        let task = VisionTask::Describe;
        let (raw, mut record) = match backend {
            Backend::Local(b) => {
                let request = DescribeRequest::new(local_images(descriptor, prepared)?, spec);
                self.metered(cx, task, descriptor, inputs, hold, b.describe(cx, &request)).await?
            }
            Backend::Remote(b) => {
                let request = DescribeRequest::new(released_images(descriptor, prepared)?, spec);
                self.metered(cx, task, descriptor, inputs, hold, b.describe(cx, &request)).await?
            }
        };
        let mut regions = Vec::with_capacity(raw.regions.len());
        for found in raw.regions {
            let normalizer = normalizers.get(found.image).ok_or_else(|| VisionError::Parse {
                backend: descriptor.id.to_string(),
                message: format!(
                    "a region names image {}, but the request had {}",
                    found.image,
                    normalizers.len()
                ),
            })?;
            if let Some(region) = normalizer.region(found.region)? {
                regions.push(region);
            }
        }
        record.regions = regions.len();
        calls.push(record);
        Ok(Description {
            text: raw.text,
            value: raw.value,
            regions,
            confidence: raw.confidence,
            escalated_from: None,
        })
    }

    async fn prepare_all(
        &self,
        cx: &VisionCx,
        descriptor: &BackendDescriptor,
        remote: bool,
        images: &[&SourceImage],
        options: &CallOptions,
    ) -> Result<Vec<Prepared>> {
        let mut prepared = Vec::with_capacity(images.len());
        for image in images {
            prepared
                .push(self.prepare(cx, descriptor, remote, image, options.region, options).await?);
        }
        Ok(prepared)
    }

    /// Embeds source images with the embedding route's backend.
    ///
    /// # Errors
    ///
    /// As [`detect_with`](Self::detect_with), and [`VisionError::Parse`] when the backend
    /// returns the wrong number of vectors or vectors outside its space's dimensions.
    pub async fn embed_images(
        &self,
        cx: &VisionCx,
        images: &[&SourceImage],
        options: &CallOptions,
    ) -> Result<VisionOutput<Vec<Vec<f32>>>> {
        let route = self.route_for(VisionTask::Embed, options, self.embedders.default_id())?;
        let (descriptor, backend) = self.embedders.get(&route.primary)?;
        let sizes: Vec<(u32, u32)> = images
            .iter()
            .map(|i| planned_size(i.size(), options.region, &descriptor.limits))
            .collect();
        let hold = self
            .preflight(cx, VisionTask::Embed, descriptor, &sizes, 0, Some(0), options.image_unit())
            .await?;
        let prepared =
            self.prepare_all(cx, descriptor, backend.is_remote(), images, options).await?;
        let inputs = Inputs::of(&prepared);
        let task = VisionTask::Embed;
        let (vectors, dims, record) = match backend {
            Backend::Local(b) => {
                let images = local_images(descriptor, prepared)?;
                let (v, r) = self
                    .metered(cx, task, descriptor, inputs, hold, b.embed_images(cx, &images))
                    .await?;
                (v, b.space().dimensions, r)
            }
            Backend::Remote(b) => {
                let images = released_images(descriptor, prepared)?;
                let (v, r) = self
                    .metered(cx, task, descriptor, inputs, hold, b.embed_images(cx, &images))
                    .await?;
                (v, b.space().dimensions, r)
            }
        };
        check_vectors(descriptor, &vectors, images.len(), dims)?;
        Ok(VisionOutput { value: vectors, calls: vec![record] })
    }

    /// Embeds texts with the embedding route's backend, into the same space as its images.
    ///
    /// # Errors
    ///
    /// As [`embed_images`](Self::embed_images).
    pub async fn embed_texts(
        &self,
        cx: &VisionCx,
        texts: &[&str],
        options: &CallOptions,
    ) -> Result<VisionOutput<Vec<Vec<f32>>>> {
        let route = self.route_for(VisionTask::Embed, options, self.embedders.default_id())?;
        let (descriptor, backend) = self.embedders.get(&route.primary)?;
        let chars = texts.iter().map(|t| t.len()).sum();
        let hold = self
            .preflight(cx, VisionTask::Embed, descriptor, &[], chars, Some(0), options.image_unit())
            .await?;
        let inputs = Inputs::default();
        let task = VisionTask::Embed;
        let (vectors, dims, record) = match backend {
            Backend::Local(b) => {
                let (v, r) = self
                    .metered(cx, task, descriptor, inputs, hold, b.embed_texts(cx, texts))
                    .await?;
                (v, b.space().dimensions, r)
            }
            Backend::Remote(b) => {
                let (v, r) = self
                    .metered(cx, task, descriptor, inputs, hold, b.embed_texts(cx, texts))
                    .await?;
                (v, b.space().dimensions, r)
            }
        };
        check_vectors(descriptor, &vectors, texts.len(), dims)?;
        Ok(VisionOutput { value: vectors, calls: vec![record] })
    }

    /// Edits a source image with the edit route's backend and stores the result with
    /// `Provenance::Derived`, under the context's media scope.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] without a media store or a media scope, as
    /// [`detect_with`](Self::detect_with) otherwise.
    pub async fn edit(
        &self,
        cx: &VisionCx,
        image: &SourceImage,
        operation: &EditOp,
        options: &CallOptions,
    ) -> Result<VisionOutput<Edited>> {
        let store = self.store.as_ref().ok_or_else(|| {
            VisionError::InvalidInput("edits are stored; give the runtime a media store".into())
        })?;
        let scope = cx.media_scope().ok_or_else(|| {
            VisionError::InvalidInput("edits are stored; give the context a media scope".into())
        })?;
        let route = self.route_for(VisionTask::Edit, options, self.editors.default_id())?;
        let (descriptor, backend) = self.editors.get(&route.primary)?;
        let sent = planned_size(image.size(), None, &descriptor.limits);
        let hold = self
            .preflight(cx, VisionTask::Edit, descriptor, &[sent], 0, None, options.image_unit())
            .await?;
        let prepared =
            self.prepare(cx, descriptor, backend.is_remote(), image, None, options).await?;
        let work = match (backend, &prepared) {
            (Backend::Local(b), Prepared::Local(s)) => b.edit(cx, s, operation),
            (Backend::Remote(b), Prepared::Remote(r)) => b.edit(cx, r, operation),
            _ => return Err(mismatch(descriptor)),
        };
        let inputs = Inputs::of(std::slice::from_ref(&prepared));
        let (pixels, record) =
            self.metered(cx, VisionTask::Edit, descriptor, inputs, hold, work).await?;
        let operation_digest = Digest::of_json(&serde_json::json!({
            "op": operation,
            "chain": prepared.image().chain(),
        }));
        let provenance = Provenance::Derived { from: image.digest(), operation: operation_digest };
        let (stored, source) = store.put_pixels(scope, pixels, provenance).await?;
        tracing::info!(
            edit.input = %image.digest(),
            edit.operation = %operation_digest,
            edit.output = %stored.digest,
            "image edited"
        );
        Ok(VisionOutput { value: Edited { image: stored, source }, calls: vec![record] })
    }
}

fn escalation_options(escalation: &Escalation, options: &CallOptions) -> CallOptions {
    let mut esc = options.clone();
    esc.route = None;
    if let Some(class) = &escalation.data_class {
        esc.data_class = Some(class.clone());
    }
    esc
}

fn local_images(descriptor: &BackendDescriptor, prepared: Vec<Prepared>) -> Result<Vec<SentImage>> {
    prepared
        .into_iter()
        .map(|p| match p {
            Prepared::Local(image) => Ok(image),
            Prepared::Remote(_) => Err(mismatch(descriptor)),
        })
        .collect()
}

fn released_images(
    descriptor: &BackendDescriptor,
    prepared: Vec<Prepared>,
) -> Result<Vec<crate::egress::ReleasedImage>> {
    prepared
        .into_iter()
        .map(|p| match p {
            Prepared::Remote(released) => Ok(*released),
            Prepared::Local(_) => Err(mismatch(descriptor)),
        })
        .collect()
}

fn check_vectors(
    descriptor: &BackendDescriptor,
    vectors: &[Vec<f32>],
    expected: usize,
    dims: usize,
) -> Result<()> {
    let parse =
        |message: String| VisionError::Parse { backend: descriptor.id.to_string(), message };
    if vectors.len() != expected {
        return Err(parse(format!("{} vectors for {expected} inputs", vectors.len())));
    }
    if let Some(v) = vectors.iter().find(|v| v.len() != dims) {
        return Err(parse(format!(
            "a {}-dimensional vector in a {dims}-dimensional space",
            v.len()
        )));
    }
    Ok(())
}

/// Maps segmentation prompts from source pixels into the backend's frame.
fn prompts_for(
    descriptor: &BackendDescriptor,
    image: &SentImage,
    prompts: &[SegmentPrompt],
) -> Result<Vec<SegmentPrompt>> {
    let chain = image.chain();
    let sent = image.size();
    let frame = descriptor.frame;
    let to_frame = |p: [f32; 2]| {
        let s = chain.to_target([f64::from(p[0]), f64::from(p[1])]);
        let f = frame.point_from_sent(s, sent);
        [f[0] as f32, f[1] as f32]
    };
    prompts
        .iter()
        .map(|prompt| {
            Ok(match prompt {
                SegmentPrompt::Concept { text } => SegmentPrompt::Concept { text: text.clone() },
                SegmentPrompt::Point { at, positive } => {
                    SegmentPrompt::Point { at: to_frame(*at), positive: *positive }
                }
                SegmentPrompt::Box { bbox } => {
                    let s = chain.bbox_to_target(bbox);
                    let a = frame.point_from_sent([f64::from(s.x0), f64::from(s.y0)], sent);
                    let b = frame.point_from_sent([f64::from(s.x1), f64::from(s.y1)], sent);
                    SegmentPrompt::Box { bbox: BBox::new(a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32) }
                }
                SegmentPrompt::Mask { mask } => {
                    let Shape::Mask(in_sent) = chain.shape_to_target(&Shape::Mask(mask.clone()))? else {
                        return Err(VisionError::InvalidInput("a mask prompt did not map to a mask".into()));
                    };
                    let mask = match frame.mask_raster(sent) {
                        Some(raster) if raster == sent => in_sent,
                        Some(raster) => resample(
                            &in_sent,
                            raster,
                            |p| frame.point_to_sent(p, sent),
                            |p| frame.point_from_sent(p, sent),
                        ),
                        None => {
                            return Err(VisionError::Unsupported(format!(
                                "backend `{}` answers in a frame with no raster, so it takes no mask prompt",
                                descriptor.id
                            )));
                        }
                    };
                    SegmentPrompt::Mask { mask }
                }
            })
        })
        .collect()
}

/// Maps a page's blocks, words and cells to source pixels, dropping those outside the image
/// and renumbering the reading order.
fn normalize_page(
    normalizer: &RegionNormalizer,
    raw: DocumentPage<RawRegion>,
) -> Result<DocumentPage> {
    let mut index_map = Vec::with_capacity(raw.blocks.len());
    let mut blocks = Vec::with_capacity(raw.blocks.len());
    for block in raw.blocks {
        let Some(region) = normalizer.region(block.region)? else {
            index_map.push(None);
            continue;
        };
        let mut words = Vec::with_capacity(block.words.len());
        for word in block.words {
            if let Some(region) = normalizer.region(word.region)? {
                words.push(Word { region, text: word.text, confidence: word.confidence });
            }
        }
        let mut cells = Vec::with_capacity(block.cells.len());
        for cell in block.cells {
            if let Some(region) = normalizer.region(cell.region)? {
                cells.push(TableCell {
                    region,
                    row: cell.row,
                    col: cell.col,
                    row_span: cell.row_span,
                    col_span: cell.col_span,
                    text: cell.text,
                });
            }
        }
        index_map.push(Some(blocks.len()));
        blocks.push(Block {
            kind: block.kind,
            region,
            text: block.text,
            confidence: block.confidence,
            words,
            cells,
        });
    }
    let reading_order =
        raw.reading_order.iter().filter_map(|&i| index_map.get(i).copied().flatten()).collect();
    Ok(DocumentPage { blocks, reading_order, markdown: raw.markdown })
}

/// An escalation the egress gate refused — a deny rule, an unlisted host, the on-device
/// profile — leaves the primary's answer standing; the gate has recorded the refusal and its
/// reason. A hold is not a refusal: it stops the call until a person decides.
fn escalation_refused(escalation: &Escalation, err: &VisionError) {
    tracing::warn!(
        vision.backend = %escalation.backend,
        error = %err,
        "escalation refused at the egress gate, the on-device answer stands"
    );
}
