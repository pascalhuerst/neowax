use num_traits::{NumCast, Zero};

#[derive(Clone, Copy, Debug)]
pub struct GenericAudioBuffer<T, const SAMPLES_PER_BUFFER: usize>
where
    T: Copy + Zero + NumCast,
{
    samples: [T; SAMPLES_PER_BUFFER],
    index: usize,
}

impl<T, const SAMPLES_PER_BUFFER: usize> Default for GenericAudioBuffer<T, SAMPLES_PER_BUFFER>
where
    T: Copy + Zero + NumCast,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const SAMPLES_PER_BUFFER: usize> GenericAudioBuffer<T, SAMPLES_PER_BUFFER>
where
    T: Copy + Zero + NumCast,
{
    pub fn new() -> Self {
        Self {
            samples: [T::zero(); SAMPLES_PER_BUFFER],
            index: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.index
    }

    pub fn is_empty(&self) -> bool {
        self.index == 0
    }

    pub fn push(&mut self, sample: T) {
        self.samples[self.index] = sample;
        self.index = (self.index + 1) % SAMPLES_PER_BUFFER;
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.index == 0 {
            None
        } else {
            self.index -= 1;
            Some(self.samples[self.index])
        }
    }

    pub fn clear(&mut self) {
        self.samples.fill(T::zero());
        self.index = 0;
    }

    pub const fn byte_size() -> usize {
        std::mem::size_of::<T>() * SAMPLES_PER_BUFFER
    }

    pub fn get_all(&self) -> &[T] {
        &self.samples
    }
}
